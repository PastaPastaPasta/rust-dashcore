> **EMBARGO-1:** an unfixed Dash Core security issue, referred to only by this codename. EMBARGO-1: see ~/private/embargo/EMBARGO-1.md (access per PLAN §7). Scrubbed by A1-rev1 on 2026-10-08.

# R2 — Dash Core inventory (target spec for the Rust full node)

**What this is:** the authoritative inventory of what Dash Core does. It is the target spec for turning rust-dashcore into a full Dash node with consensus parity, network indistinguishability, masternode operation, dash-cli compatibility and user-story parity.

**Source surveyed:** `~/workspace/dash`, branch `develop`, commit `15db478750` (2026-10-06). It reports itself as **v24.1.0 (pre-release)**, `PROTOCOL_VERSION = 70242`, `MIN_PEER_PROTO_VERSION = 70221`. The latest tag is `v24.0.0-rc.2` (2026-09-30) and is an ancestor of HEAD. That release candidate already contains the V24 features described below and the same protocol version.

**Method:** this was a read-only survey; nothing in either repo was modified. Seven parallel research passes covered: core validation; P2P; evo and masternodes; LLMQ, ChainLocks, InstantSend and BLS; governance and CoinJoin; wallet, RPC and config; data formats and tests. I verified spot facts myself and extracted the RPC table mechanically from the `CRPCCommand` tables. File/line citations (`path:line`) refer to the Dash Core tree. They will drift as develop moves, so re-verify before relying on an exact line.

**Companion file:** [`R2b-rpc-inventory.md`](R2b-rpc-inventory.md) — the full RPC table, 289 distinct commands.

## 0. Executive summary

1. **Scale.**
   - Dash Core is about **236k lines** of non-test, non-vendored C++ under `src/` (excluding Qt at 51k, vendored `dashbls` at 63k, secp256k1, leveldb and similar).
   - About 60k of that is Dash-specific:
     - `evo/` 14.5k
     - `llmq/` 15.2k
     - `chainlock/` + `instantsend/` 4.3k
     - `governance/` 5.5k
     - `coinjoin/` 7.1k
     - `masternode/` + `active/` + `spork` 4k
     - `bls/` 2.6k
     - Dash RPC files about 7k
   - Everything else is a Bitcoin Core v22–v26-era base: chainstate, assumeutxo scaffolding, BIP324, descriptor wallets, `getdeploymentinfo`.
2. **Consensus surface beyond Bitcoin.** A Rust node needs all of these to stay on the chain:
   - X11 PoW with KGW and then DGW v3 (including the historical off-by-one and IEEE-double maths).
   - Dash's difficulty-dependent subsidy, also floating point, using the *previous* block's bits and height.
   - 13 special-transaction types (DIP2 framing) with their payload versions.
   - The deterministic masternode list (DIP3): a stateful per-block state machine with PoSe and payee selection.
   - CbTx v1–v4, carrying the MN-list merkle root, quorums merkle root, best ChainLock, credit-pool balance and asset-unlock root.
   - LLMQ final commitments: exact per-block commitment counts and DKG mining windows.
   - DIP24 rotated quorum member selection, which depends on snapshots created as a side effect of block connection.
   - BLS legacy-vs-basic serialization switching at V19. This includes a non-standard legacy hash-to-G2 that no mainstream crate implements.
   - ChainLock-conflict and InstantSend-conflict block rejection.
   - Masternode and superblock payment rules.
   - Credit pool and asset lock/unlock limits.
   - MNHF/EHF-gated versionbits with a dynamic threshold.
   - The re-enabled DIP0020 opcodes and `OP_CHECKDATASIG`.
   - V24 shared-collateral covenants.
3. **Some block validity depends on node state.** Superblock validity depends on locally synced governance data. MN-payee and superblock checks are skipped for ChainLocked blocks or when not synced. Conflicting-ISLOCK rejection only applies outside IBD. Blocks conflicting with the best ChainLock are rejected. A Rust node therefore cannot be a pure function of the block data. It must run governance, masternode-sync, ChainLock and InstantSend state machines that converge the way dashd's do.
4. **Activation surface** (§C). There are 15 buried deployments, from bip34 through `withdrawals`. All are buried on mainnet, the last at 2201472. There is one live BIP9/EHF deployment, **`v24`** (bit 13).
   - V24 is defined but `NEVER_ACTIVE` on mainnet (TODO). Its start time on testnet is 2026-09-24.
   - V24 gates:
     - shared masternodes: ProDisTx (type 10), ProUpShareTx (11), ProUpSharedRegTx (12), and ProRegTx v3 share tables
     - ExtNetInfo (ProTx v3) and multi-payouts
     - CbTx v4
     - asset-lock/unlock v2 with a "stable txid" (the txid excludes the quorum signature)
     - a doubled withdrawal limit
     - CoinJoin rebalance sessions
     - strict payee multiplicity
   - The Rust node must implement both the pre-V24 and the V24 rules.
5. **LLMQs.** There are 14 types (6 production, 8 test/devnet). Mainnet uses 400_60 for ChainLocks, 60_75 (rotating) for InstantSend, 100_67 for Platform and asset unlocks, and 400_85 for EHF. Per-network assignments and devnet/regtest overrides are in §C.6.
6. **P2P.**
   - 33 Bitcoin-heritage commands and 41 Dash commands, with BIP324 v2 short IDs 128–168.
   - 19 live inventory types (`MSG_*`; the remaining enum slots are retired).
   - Fingerprint-relevant details are in A.2's closing notes. Examples:
     - The `version` message carries an extra mnauth challenge and masternode flag.
     - `NODE_BLOOM` and `NODE_HEADERS_COMPRESSED` are on by default.
     - `headers2` compressed headers.
     - Automatic `getsporks`, `mempool` and `govsync` traffic.
     - Score-based misbehavior with a ban threshold of 100.
     - No `feefilter`, `wtxidrelay` or `reject`.
     - The BIP324 HKDF salt includes the Dash magic.
7. **RPC.** 289 distinct commands (§B, R2b), 115 of them Dash-specific. Dash uses two-word subcommands such as `protx register` and `quorum sign`, dispatched as `method + " " + params[0]`. Platform, Tenderdash and Drive depend on a stable subset: `quorum *`, `protx diff`, `masternodelist`, `getbestchainlock`, `getassetunlockstatuses`, ZMQ chainlock/recsig topics, and others. There is no JSON-RPC 2.0.
8. **Wallet.**
   - Descriptor (SQLite) wallets are the default. Their BIP39 mnemonic is stored with the descriptor key, which is a Dash extension.
   - Legacy wallets are BDB 4.8 with Dash's own `hdchain`/`chdchain` record: seed, mnemonic, passphrase and per-account counters, with keys re-derived and never stored.
   - Import quirks that must be reproduced, or some existing wallets will not import:
     - BIP39 checksum validation is weakened (it uses XOR instead of a power).
     - The passphrase is not NFKD-normalised.
     - CoinJoin "fully mixed" status depends on `cj_salt`.
   - Only Knapsack coin selection is effective. There is no RBF.
9. **Import from Dash Core** (§E).
   - **Must import as a set agreeing on one tip:** `blocks/` (plaintext, no xor.dat), `blocks/index`, `chainstate/` (XOR-obfuscated values) and `evodb/`.
   - **Nice to have:** `llmq/quorumdb`, `llmq/isdb` and `governance.dat`.
   - **Wallets:** must import. SQLite is easy. BDB needs a page parser or the `dash-wallet dump` route.
   - **Regenerable:** indexes and the remaining flat files.
   - **Format quirks a Rust reader will trip on:**
     - `CDiskBlockIndex` stores the X11 hash inline.
     - The undo checksum uses the *previous* block hash.
     - The BIP158 filters are Dash-extended and are not BIP158-spec.
     - Evo, LLMQ and IS LevelDB keys use CompactSize-prefixed string tags.
10. **Test assets** (§F).
    - 264 functional tests, about 56 of them Dash-specific, plus `DashTestFramework` and a Python Dash P2P codec in `messages.py`.
    - Dash-specific JSON vectors: `trivially_(in)valid.json`, `proposals_*.json`, BIP39, and blockfilters recomputed with X11.
    - Real-data unit tests for DGW, subsidy, ISLOCK, SML and quorum proofs.
    - Only one Dash-specific fuzz target exists, so special-tx, ISLOCK and governance fuzz targets would have to be written.
    - The functional suite can drive a Rust node directly if it accepts dashd's regtest and devnet flags.
11. **Biggest risks for the program, ranked:**
    1. BLS legacy scheme plus scheme-switch semantics.
    2. DIP24 rotation and snapshot side effects.
    3. Deterministic MN-list state machine and evodb byte compatibility (immer HAMT iteration order).
    4. Floating-point consensus (subsidy, KGW).
    5. State-dependent validity (governance, ChainLocks, IS).
    6. Network fingerprint parity.
    7. Moving target: develop is ahead of the v24.0.0-rc.2 tag. V24 still needs to be defined on mainnet, and rust-dashcore lacks tx types 10–12, ProTx v3, CbTx v4 and asset lock/unlock v2.

## Contents

- [A. Subsystem map](#a-subsystem-map)
  - A.0 overview table
  - A.1 validation, chainstate, coins, script, PoW, mining, mempool, indexes, deployments, block storage
  - A.2 P2P, net_processing, addrman
  - A.3 evo, masternodes, sporks
  - A.4 LLMQ, ChainLocks, InstantSend, BLS
  - A.5 governance and CoinJoin
  - A.6 wallet, RPC server, REST, ZMQ, CLI, init/config, stats, GUI
- [B. RPC inventory (summary)](#b-rpc-inventory-summary) — full table in R2b
- [C. Consensus parameter and activation surface](#c-consensus-parameter--activation-surface)
- [D. User-story catalog](#d-user-story-catalog-target-parity-surface)
- [E. Data formats and import](#e-data-formats-and-import)
- [F. Test assets](#f-test-assets)
- [G. Cross-cutting pitfalls, inconsistencies found, open questions](#g-cross-cutting-pitfalls-inconsistencies-found-open-questions)

---

## A. Subsystem map

Each subsection below covers a group of subsystems. For each it gives source paths and line counts, what the code does, what is consensus-critical (exact constants and rules), the persistent state it owns, and notes for the Rust reimplementation.

---

### A.0 Overview table

Line counts are `wc -l` over `.cpp`/`.h` files (tests excluded) at develop `15db478750`. "Consensus" means a divergence forks you off the network or makes you reject valid blocks. "Network-visible" means peers can observe or punish the behaviour, but it does not fork you.

| # | Subsystem | Main source paths | ~Lines | Consensus? | Persistent state owned | Detail |
|---|---|---|---|---|---|---|
| 1 | Validation / chainstate / coins / block storage | `validation.*`, `coins.*`, `txdb.*`, `undo.h`, `chain.*`, `consensus/*`, `node/blockstorage.*`, `node/chainstate.*`, `kernel/*`, `evo/chainhelper.*` | 14.6k | **Yes** (core) | `blocks/blk*.dat`, `rev*.dat`, `blocks/index/` (LevelDB), `chainstate/` (LevelDB, XOR-obfuscated) | A.1 §1, §8 |
| 2 | Script interpreter and flags | `script/*` | 8.3k | **Yes** | sig and script caches (memory only) | A.1 §2 |
| 3 | PoW (KGW → DGW v3), X11 | `pow.*`, `hash_x11.h`, `crypto/x11/*` | 0.4k + 11.7k | **Yes** | – | A.1 §3 |
| 4 | Block assembly / GBT / MN and superblock payments | `node/miner.*`, `rpc/mining.cpp`, `masternode/payments.*`, `governance/superblock.*` | 2.1k + 0.6k | **Yes** (payee and value rules) | – | A.1 §4, A.3 §9, A.5 §2.9–2.11 |
| 5 | Mempool and fee policy | `txmempool.*`, `policy/*`, `txorphanage.*`, `kernel/mempool_*` | 6.0k | Policy, but IS/CL-adjacent | `mempool.dat` (v1, no XOR), `fee_estimates.dat` | A.1 §5 |
| 6 | P2P: net, net_processing, transport, protocol | `net.*`, `net_processing.*`, `protocol.*`, `netbase.*`, `netaddress.*`, `bip324.*`, `i2p.*`, `torcontrol.*`, `mapport.*`, `node/eviction.*`, `txrequest.*`, `blockencodings.*` | 22.7k | Network-visible | `onion_v3_private_key`, `i2p_private_key`, `anchors.dat` | A.2 |
| 7 | addrman / banman | `addrman*`, `addrdb.*`, `banman.*`, `netgroup.*` | 2.7k | Network-visible | `peers.dat` (format 4), `banlist.json` | A.2 §5 |
| 8 | evo: special txs, deterministic MN list, SML, CbTx, credit pool, asset lock/unlock, MNHF, netinfo, shared MNs, mnauth, evo snapshot | `evo/*` | 14.5k | **Yes** | `evodb/` (LevelDB) | A.3 |
| 9 | Masternode payments, sync, meta, utils | `masternode/*`, `node/sync_manager.*` | 1.9k | Payments: **Yes**. Sync/meta: network-visible | `mncache.dat`, `netfulfilled.dat` | A.3 §9, §13 |
| 10 | Active masternode (MN-only roles) | `active/*` (incl. `active/dkgsession*`, `active/masternode.*`, `active/context.*`) | 1.9k | Network-visible (DKG participation) | – | A.3 §12, A.4 |
| 11 | Sporks | `spork.*` | 0.6k | Indirect (mainnet values hard-coded, see A.3 §14) | `sporks.dat` | A.3 §14 |
| 12 | LLMQ: params, DKG, commitments, quorum manager, signing, sig shares, recovered sigs, rotation/snapshots, data recovery, EHF signing, quorum proofs | `llmq/*` | 15.2k | **Yes** (commitments in blocks, quorum selection, sign hashes) | `evodb` keys `q_*`/`llmq_S`, `llmq/quorumdb`, `llmq/dkgdb`, `llmq/recsigdb` | A.4 |
| 13 | ChainLocks | `chainlock/*` | 1.5k | **Yes** (block rejection, CbTx bestCL) | none (in memory; CbTx carries history) | A.4 |
| 14 | InstantSend | `instantsend/*` | 2.8k | **Yes**: conflicting-lock block rejection outside IBD; mempool policy | `llmq/isdb` | A.4 |
| 15 | BLS wrapper and vendored lib | `bls/*` (wrapper, worker, IES), `dashbls/` (vendored Chia BLS fork, ~63k lines incl. relic) | 2.6k + 63k | **Yes** (legacy vs basic serialization, verification) | – | A.4 |
| 16 | Governance | `governance/*`, `rpc/governance.cpp` | 5.5k | Superblock payments: **Yes**, but state-dependent. Objects/votes: policy | `governance.dat` | A.5 §2 |
| 17 | CoinJoin client and server | `coinjoin/*`, `wallet/coinjoin.*` | 7.1k | No (policy and P2P). DSTX relay rules are network-visible | wallet keys `cj_salt`, `cj_pending_obs` | A.5 §3 |
| 18 | Indexes: txindex (default **on**), addressindex, spentindex, timestampindex, blockfilterindex (Dash-extended BIP158), coinstatsindex | `index/*`, `blockfilter.*`, `evo/specialtx_filter.*` | 3.5k + 0.4k | No, but BIP158 filters served to peers must match | `indexes/*` (LevelDB) + `indexes/blockfilter/basic/fltr*.dat` | A.1 §6, E.7 |
| 19 | Wallet: legacy (BDB, BIP39/BIP44 `hdchain`) and descriptor (SQLite, default); CoinJoin integration; PSBT; external signer | `wallet/*` | 33.1k | No | `wallets/<name>/wallet.dat` (BDB 4.8 or SQLite), `backups/` | A.6 §1 |
| 20 | RPC server and HTTP | `rpc/server.*`, `rpc/util.*`, `rpc/request.*`, `httprpc.*`, `httpserver.*` (all RPC handlers: `rpc/*` 22.2k) | 4.1k | No (but compatibility-critical for dash-cli and Platform) | `.cookie` | A.6 §2, B |
| 21 | REST | `rest.*` | 1.1k | No | – | A.6 §3 |
| 22 | ZMQ | `zmq/*` | 1.2k | No | – | A.6 §4 |
| 23 | Init / config / chainparams | `init.*`, `init/*`, `common/*`, `util/system.*`, `chainparams*` | 7.9k | chainparams: **Yes** | `dash.conf`, `settings.json`, `.lock`, `dashd.pid` | A.6 §6, C |
| 24 | Stats client (statsd) | `stats/*` | 0.6k | No | – | A.6 §7 |
| 25 | Qt GUI | `qt/*` | 50.8k | No | GUI settings (QSettings), `settings.json` | A.6 §9 |
| 26 | Tools: dash-cli, dash-tx, dash-util, dash-wallet, dash-chainstate | `bitcoin-cli.cpp`, `bitcoin-tx.cpp`, `bitcoin-util.cpp`, `bitcoin-wallet.cpp`, `wallet/wallettool.*`, `bitcoin-chainstate.cpp` | ~3k | No | – | A.6 §5 |

**How the Dash systems hang together at runtime.** `PeerManagerImpl::ProcessMessage` handles the Bitcoin messages and the Dash messages it owns directly: spork, getsporks, senddsq, getmnlistd, getqrinfo, headers2, platformban, clsig. Everything else goes to a chain of `NetHandler`s registered in `init.cpp:2208-2281`:

- `NetInstantSend`
- `llmq::NetSigning`
- `llmq::NetQuorum`
- `llmq::NetDKG` (or `NetDKGStub` on non-masternodes)
- `CCoinJoinServer` (masternode only)
- `NetGovernance`
- `SyncManager`

Mixing messages from wallet peers go to the CoinJoin wallet manager.

Validation calls into evo through `CChainstateHelper` (`evo/chainhelper.*`). That helper bundles `CSpecialTxProcessor`, `CMNPaymentsProcessor`, the credit pool, EHF, and superblocks. It also queries ChainLocks and InstantSend.

Masternode-only components (DKG sessions, sig-share signer, ChainLock/IS/governance/EHF signers, CoinJoin server) live in `ActiveContext` (`active/context.*`). They are only constructed when `-masternodeblsprivkey` is set.

### A.1 Validation, chainstate, coins, script, PoW, mining, mempool, indexes, deployments, block storage

*Section numbers below (§1, §2, …) are local to A.1.*

All paths below are under `/work/home/agent/workspace/dash/` (branch `develop`, HEAD `15db478750`). For example, `src/validation.cpp` means `/work/home/agent/workspace/dash/src/validation.cpp`. Line counts come from `wc -l`.

Three things about this tree surprised me and matter for the spec:
- **Address, spent and timestamp indexes have moved.** They are now asynchronous `BaseIndex` databases under `indexes/`. The old records in the block-tree DB (`blocks/index`) are migrated out at startup. Both layouts are documented in §6.
- **Asset-unlock txids changed.** Version-2 asset-unlock transactions have a "stable" txid that excludes the quorum signature. The coinbase commits to their full-serialization hashes instead.
- **DIP0020 opcodes are always on.** `OP_CAT` and the rest are enabled from genesis; there is no script flag for them anymore.

---

#### 1. Validation, chainstate and coins

**Files:**

| File | Lines |
|---|---|
| `src/validation.cpp` | 6996 |
| `src/validation.h` | 1312 |
| `src/coins.cpp` / `src/coins.h` | 379 / 393 |
| `src/txdb.cpp` / `src/txdb.h` | 645 / 102 |
| `src/undo.h` | 71 |
| `src/chain.h` | 504 |
| `src/consensus/tx_check.cpp` | 75 |
| `src/consensus/tx_verify.cpp` | 208 |
| `src/consensus/consensus.h` | 30 |
| `src/consensus/amount.h` | 29 |
| `src/primitives/transaction.{h,cpp}` | 436 / 163 |
| `src/primitives/block.{h,cpp}` | 274 / 134 |
| `src/evo/chainhelper.{h,cpp}` | 92 / 104 |
| `src/evo/specialtxman.cpp` (`ProcessSpecialTxsInBlock`, around line 800) | 1868 |

**What it does.** `Chainstate` (Bitcoin v25+ naming; there is no `CChainState`) and `ChainstateManager` are the usual Bitcoin design. This includes assumeutxo snapshot support: a `chainstate_snapshot/` directory, a `base_blockhash` file, and `RecordBackgroundMNListHash` so the masternode list is checked when the background chainstate catches up. Dash adds a `CChainstateHelper` (`m_chain_helper`) that bundles:
- `special_tx` (`CSpecialTxProcessor`)
- `mn_payments` (`CMNPaymentsProcessor`)
- `credit_pool_manager`
- `ehf_manager` (`CMNHFManager`)
- `superblocks`
- pass-throughs to ChainLocks and InstantSend (`HasConflictingChainLock`, `HasChainLock`, `ConflictingISLockIfAny`, `IsInstantSendWaitingForTx`, …)

There is a second state store, **evodb**. `ConnectBlock`/`DisconnectBlock` call `m_evoDb.VerifyBestBlock` and `WriteBestBlock`. `FlushStateToDisk` flushes the coins DB first and then `m_evoDb.CommitRootTransaction` (`validation.cpp:3089-3097`). A full flush is also forced when the evodb cache reaches 64 MiB or more.

##### Transaction format and consensus rules

**Serialization** (`primitives/transaction.h:242-363`):
- The first 4-byte field holds `nVersion` (low 16 bits) and `nType` (high 16 bits).
- Then `vin`, `vout`, `nLockTime`.
- Then `vExtraPayload` (a compact-size byte vector), present only if `nVersion >= 3 && nType != 0`.
- There is no witness: SegWit was never adopted. `CURRENT_VERSION = 2`, `SPECIAL_VERSION = 3`.

**Special transaction types** (`transaction.h:29-41`):

| Value | Type |
|---|---|
| 0 | NORMAL |
| 1 | PROVIDER_REGISTER |
| 2 | PROVIDER_UPDATE_SERVICE |
| 3 | PROVIDER_UPDATE_REGISTRAR |
| 4 | PROVIDER_UPDATE_REVOKE |
| 5 | COINBASE |
| 6 | QUORUM_COMMITMENT |
| 7 | MNHF_SIGNAL |
| 8 | ASSET_LOCK |
| 9 | ASSET_UNLOCK |
| 10 | PROVIDER_DISSOLVE |
| 11 | PROVIDER_UPDATE_SHARE |
| 12 | PROVIDER_UPDATE_SHARED_REGISTRAR |

**Txid** (`transaction.cpp:75-123`):
- Normally the txid is double-SHA256 of the serialization.
- Exception: an asset unlock with `vExtraPayload.size() == ASSET_UNLOCK_PAYLOAD_SIZE (1+8+4+132 = 145)` and `payload[0] >= 2`. Its txid is computed after **zeroing the last `ASSET_UNLOCK_QUORUM_INFO_SIZE = 4+32+96 = 132` bytes** of the serialized stream.
- `GetInstanceHash()` is the full-serialization hash.

**`CheckTransaction`** (`consensus/tx_check.cpp`), context-free:
- `vin` may be empty only if `nType ∈ {6, 7, 9}`; `vout` may be empty only if `nType ∈ {6, 7}`. These checks use `nType` regardless of `nVersion`.
- Serialized size must be ≤ `MAX_LEGACY_BLOCK_SIZE = 1,000,000`.
- `vExtraPayload.size()` must be ≤ `MAX_TX_EXTRA_PAYLOAD = 10000`.
- Each output must satisfy 0 ≤ value ≤ `MAX_MONEY` (21,000,000·10⁸), and the running sum must stay within `MoneyRange`.
- No duplicate inputs.
- Coinbase scriptSig length must be 2..100, or **1..100 when `nType == 5` (CbTx)**.
- Non-coinbase inputs must not have a null prevout.

**`ContextualCheckTransaction`** (`validation.cpp:351-386`), evaluated against the next block's height:
- Once DIP0003 is active: a version-3+ tx must have `nType` ∈ {0..12}, otherwise `bad-txns-type`. A coinbase must have `nType == 5` (`bad-cb-type`). A tx with version < 3 and `nType != 0` is rejected.
- Once DIP0001 is active: serialized size > `MAX_STANDARD_TX_SIZE = 100000` → `bad-txns-oversize`. **This is consensus, not policy.**

**`Consensus::CheckTxInputs`** (`tx_verify.cpp:165-208`):
- An asset unlock (`nVersion >= 3 && nType == 9`) returns `GetAssetUnlockFee(tx)` immediately and skips the input checks.
- Otherwise, the usual checks: inputs exist (`bad-txns-inputs-missingorspent`), coinbase maturity (`nSpendHeight - coin.nHeight < COINBASE_MATURITY (100)` → `bad-txns-premature-spend-of-coinbase`), input values in `MoneyRange`, `in >= out`, and the fee in `MoneyRange`.

**Lock times:**
- `IsFinalTx` is the standard rule with `LOCKTIME_THRESHOLD = 500000000`.
- BIP68: `CalculateSequenceLocks` / `EvaluateSequenceLocks` are unchanged from Bitcoin. They apply only when `(uint32)nVersion >= 2` and `LOCKTIME_VERIFY_SEQUENCE` is set (CSV active). Constants: disable flag `1<<31`, type flag `1<<22`, mask `0xffff`, granularity 9.
- BIP113: `ContextualCheckBlock` uses the median time past (MTP) as the lock-time cutoff once CSV is active.

**Sigops** (`tx_verify.cpp`):
- Legacy count: `GetSigOpCount(false)` over scriptSig and scriptPubKey. `CHECKSIG`/`CHECKSIGVERIFY` count 1; `CHECKMULTISIG`/`CHECKMULTISIGVERIFY` count 20 (inaccurate mode).
- P2SH sigops are added in `ConnectBlock` because the P2SH flag is always on.
- **`OP_CHECKDATASIG` and `OP_CHECKDATASIGVERIFY` are not counted.**

##### Block checks

**`CheckBlockHeader`:**
- `CheckProofOfWork(X11 hash)` → `high-hash`.
- Devnet rule: if `hashDevnetGenesisBlock` is non-null and `hashPrevBlock == hashGenesisBlock`, the hash must equal `hashDevnetGenesisBlock` (`devnet-genesis`).

**`CheckBlock`** (`validation.cpp:4364`):
1. Merkle root. Mismatch → `bad-txnmrklroot`; CVE-2012-2459 duplicate → `bad-txns-duplicate`; both are `BLOCK_MUTATED`. **New:** if `vtx[0]` is a CbTx with version ≥ 4, `merkleRootAssetUnlocks` must equal the merkle root of the instance hashes of all version-2+ asset unlocks in block order, otherwise `bad-cbtx-assetunlockmerkleroot` (also MUTATED).
2. Relaxed size: tx count ≤ 2,000,000 and serialized size ≤ 2,000,000.
3. First tx is coinbase and only the first.
4. `CheckTransaction` on every tx.
5. Legacy sigops ≤ `MaxBlockSigOps()` = 40,000.

**`ContextualCheckBlockHeader`** (`:4467`):
- Mainnet, height ≤ 68589: accept if `|diff(nBits) - diff(expected)| <= 0.5*diff(nBits)`, using the floating-point `ConvertBitsToDouble`.
- Otherwise `nBits` must equal `GetNextWorkRequired` exactly (`bad-diffbits`).
- Checkpoints (`bad-fork-prior-to-checkpoint`).
- `time > MTP(prev)`.
- `time <= now + MAX_FUTURE_BLOCK_TIME (7200 s)`.
- `bad-version`: version < 2 after BIP34, < 3 after BIP66, < 4 after BIP65.

**`ContextualCheckBlock`** (`:4528`):
- Size ≤ `MaxBlockSize(DIP0001 active)`: 1,000,000 before DIP0001, 2,000,000 after.
- `IsFinalTx` with the MTP cutoff once CSV is active.
- `ContextualCheckTransaction` on every tx.
- Legacy sigops ≤ `MaxBlockSize/50` (20,000 or 40,000).
- BIP34 height in the coinbase scriptSig only while BIP34 is active **and DIP0003 is not**. After DIP0003 the height is in the CbTx payload, checked by `CheckCbTx`.
- After DIP0003 the coinbase must have `nType == 5` (`bad-cb-type`).

**`AcceptBlockHeader`** (`:4596`):
- Rejects headers whose prev has `BLOCK_CONFLICT_CHAINLOCK` (`bad-prevblk-chainlock`).
- If `HasConflictingChainLock(height, hash)`, the header is still added to the index with `BLOCK_CONFLICT_CHAINLOCK` and then rejected (`bad-chainlock`).

##### `ConnectBlock` order (`validation.cpp:2470-2933`)

1. `CheckBlock` (a MUTATED result aborts the node).
2. Conflicting ChainLock → `BLOCK_CHAINLOCK` / `bad-chainlock`.
3. Evodb best block must equal `pprev` once DIP0003 is active (otherwise the node aborts and requires a reindex).
4. The genesis block only sets the best block.
5. **assumevalid**: scripts are skipped if the block is an ancestor of both the assumevalid block and `m_best_header`, best-header work ≥ `nMinimumChainWork`, and `GetBlockProofEquivalentTime(best, pindex) > 2 weeks`.
6. **BIP30**: `IsBIP30Repeat` / `IsBIP30Unspendable` always return false ("Dash blockchain does not have any BIP30 violations"). The check is enforced unless the ancestor at `BIP34Height` has hash `BIP34Hash`; then it is skipped. Failure → `bad-txns-BIP30`.
7. At `nSuperblockStartBlock`, if `nSuperblockStartHash` is non-null, the block hash must match (`bad-sb-start`).
8. Flags: `LOCKTIME_VERIFY_SEQUENCE` if CSV is active, plus script flags from `GetBlockScriptFlags` (§2).
9. `blockSubsidy = GetBlockSubsidy(pindex)`.
10. **`ProcessSpecialTxsInBlock` runs BEFORE the UTXO updates.** It covers:
    - CbTx checks (`bad-cbtx-*`)
    - V20 credit-pool checks
    - per-tx `CheckSpecialTxInner`
    - quorum block processing (`m_qblockman.ProcessBlock`)
    - `BuildNewListFromBlock` and `m_dmnman.ProcessBlock`
    - `merkleRootMNList` (`bad-cbtx-mnmerkleroot`) and `merkleRootQuorums` (`bad-cbtx-quorummerkleroot`)
    - `CheckCbTxBestChainlock`
    - `m_mnhfman.ProcessBlock`
    - switching the BLS scheme off "legacy" when V19 becomes active
    - any exception → `failed-procspectxsinblock`
11. Per tx:
    - `CheckTxInputs`
    - if V24 is active: `CheckSharedCollateralSpends`
    - accumulate fees (`bad-txns-accumulated-fee-outofrange`)
    - `SequenceLocks` (`bad-txns-nonfinal`)
    - `nSigOps += legacy + P2SH`, must stay ≤ `MaxBlockSigOps(DIP0001)` (`bad-blk-sigops`)
    - `CheckInputScripts`
    - `UpdateCoins`
12. Wait for the script check queue (`block-validation-failed`).
13. **InstantSend**, only when not in IBD: for each tx with lock inputs and a conflicting ISLOCK, if the block itself is ChainLocked the ISLOCK is dropped; otherwise the block is rejected with `BLOCK_CHAINLOCK` / `conflict-tx-lock`.
14. `IsBlockValueValid(..., subsidy + fees, check_superblock)` → `bad-cb-amount`.
15. `IsBlockPayeeValid(..., subsidy, fees, era, strict_multiplicity = V24 active, check_superblock)` → `bad-cb-payee`.
    - Both 14 and 15 fail with `BLOCK_RESULT_UNSET`: the peer is not punished, but `InvalidBlockFound` still sets `BLOCK_FAILED_VALID`.
    - `check_superblock` is `NoCheck` if the best ChainLock height ≥ this height or masternode sync is not complete. Otherwise it is `DisallowDuplicates` (V24) or `AllowDuplicates`.
16. Write the undo data, raise validity to `BLOCK_VALID_SCRIPTS`, set the best block in both the coins view and evodb.

##### Block subsidy (`validation.cpp:1785-1890`)

`GetBlockSubsidy(pindex)` returns the genesis coinbase value for the genesis block. Otherwise it calls `GetBlockSubsidyInner(prev.nBits, prev.nHeight, V20 active at pindex)`. **It uses the previous block's bits and height.**

**Difficulty used:**
- `dDiff = ConvertBitsToDouble(nPrevBits)`: `0xffff / mantissa`, shifted by ×256 or ÷256 until the exponent equals 29.
- Mainnet with `nPrevHeight <= 4500` (bug): `dDiff = 0xffff / (bits & 0xffffff)` with no exponent adjustment.

**Base subsidy** (`nSubsidyBase` is a `CAmount`, so the double is **truncated** before clamping):

| Condition | Formula | Clamp |
|---|---|---|
| V20 active, or devnet with `nPrevHeight >= nHighSubsidyBlocks` | 5 | — |
| `nPrevHeight < 5465` | `1111/(dDiff+1)^2` | [1, 500] |
| `nPrevHeight < 17000`, or (`dDiff <= 75` and `nPrevHeight < 24000`) | `11111/((dDiff+51)/6)^2` | [25, 500] |
| otherwise | `2222222/((dDiff+2600)/9)^2` | [5, 25] |

**Then:**
1. `nSubsidy = base * COIN`.
2. `for (i = 210240; i <= nPrevHeight; i += 210240) nSubsidy -= nSubsidy/14;` (integer division; the interval is `nSubsidyHalvingInterval`).
3. Devnet: multiply by `nHighSubsidyFactor` if `nPrevHeight < nHighSubsidyBlocks`.
4. If `nPrevHeight > nBudgetPaymentsStartBlock`: `superblockPart = nSubsidy / (V20 ? 5 : 10)`, i.e. 20% or 10%.
5. Return `nSubsidy - superblockPart` (miner plus masternode). The treasury part is available separately through `GetSuperblockSubsidyInner`.

**Masternode reward eras** (`GetMnRewardEraAfter`):

| Era | Condition |
|---|---|
| Classic | before V20 |
| CreditPool | V20 active, MN_RR not active |
| EvoReward | MN_RR active |

##### Chainstate LevelDB (`<datadir>/chainstate/`, obfuscated)

The obfuscation key is stored under the key `"\x00obfuscate_key"` (14 bytes). The value is an 8-byte random XOR key, serialized as a vector. All values are XORed with it, repeated; keys are not.

| Key | Value |
|---|---|
| `'C'` + txid(32, internal byte order) + VARINT(vout) | `Coin`: VARINT(height·2 + coinbase) followed by `TxOutCompression` (VARINT of `CompressAmount(value)`, then `ScriptCompression`: special types 0–5, else VARINT(len+6) + script) |
| `'B'` | uint256 best block |
| `'H'` | vector<uint256>[new_tip, old_tip], written during a partial flush |
| `'c'` | legacy pre-0.15 per-tx coins; if present, `NeedsUpgrade` → error |

The flush protocol is Bitcoin's: erase `'B'`, write `'H'`, write the batched coins, erase `'H'`, write `'B'`. `OP_RETURN` outputs and scripts larger than 10000 bytes (`IsUnspendable`) are never added to the coin view.

##### Block tree DB (`<datadir>/blocks/index/`, not obfuscated)

| Key | Value |
|---|---|
| `'b'` + hash | `CDiskBlockIndex` (see below) |
| `'f'` + int32 file number | `CBlockFileInfo`: VARINT `nBlocks, nSize, nUndoSize, nHeightFirst, nHeightLast, nTimeFirst, nTimeLast` |
| `'l'` | int last file |
| `'R'` | `'1'` while reindexing |
| `'F'` + string | `'1'`/`'0'`. Names: `prunedblockfiles`, and legacy `addressindex`, `spentindex`, `timestampindex` |
| legacy `'a'`, `'u'`, `'p'`, `'s'` | old index records (see §6) |

**`CDiskBlockIndex`** (`chain.h:394-413`):
1. VARINT(client version, non-negative signed)
2. VARINT(nHeight)
3. VARINT(nStatus)
4. VARINT(nTx)
5. VARINT(nFile) if `HAVE_DATA|HAVE_UNDO`
6. VARINT(nDataPos) if `HAVE_DATA`
7. VARINT(nUndoPos) if `HAVE_UNDO`
8. **`uint256 hash` (the X11 block hash, stored explicitly; Bitcoin does not store this)**
9. nVersion, hashPrev, hashMerkleRoot, nTime, nBits, nNonce

`LoadBlockIndexGuts` re-checks `CheckProofOfWork` for every entry.

**`nStatus` bits:**

| Value | Meaning |
|---|---|
| 0–5 | validity level (`VALID_TREE = 2`, `TRANSACTIONS = 3`, `CHAIN = 4`, `SCRIPTS = 5`) |
| 8 | `HAVE_DATA` |
| 16 | `HAVE_UNDO` |
| 32 | `FAILED_VALID` |
| 64 | `FAILED_CHILD` |
| **128** | **`BLOCK_CONFLICT_CHAINLOCK`** (Dash) |
| 256 | `ASSUMED_VALID` |

`FindMostWorkChain` treats `CONFLICT_CHAINLOCK` like a failed status and propagates it to descendants. `EnforceBlock` and `MarkConflictingBlock` (`:4046-4148`) disconnect a conflicting chain and mark it.

**Undo format:** a `CBlockUndo` is a vector of `CTxUndo`, which is a vector of `Coin`, serialized by `TxInUndoFormatter`: VARINT(height·2 + cb); if height > 0, a dummy VARINT version byte 0; then the compressed txout. There is no entry for the coinbase.

##### Notes for a Rust reimplementation
- Round-trip the 32-bit version/type field exactly as C++ serializes it.
- Use the previous block's height and bits for the subsidy. Use **C-style double math** (`pow`, truncation to int64) and the mainnet ≤ 4500 bug. Make sure your `pow` matches glibc `pow` bit-for-bit for these inputs.
- `MAX_STANDARD_TX_SIZE` (100 kB) is consensus after DIP0001.
- `CheckBlock` uses the relaxed 2 MB and 40k-sigop limits; `ContextualCheckBlock` uses the height-dependent ones.
- Special-tx processing happens before UTXO application, and the shared-collateral spend check happens inside the UTXO loop. Keep this order, because error codes and intra-block visibility differ.
- `IsBlockValueValid` and `IsBlockPayeeValid` depend on governance and masternode-sync state and on ChainLocks, so they are **non-deterministic across nodes**. A Rust node must follow ChainLocks to converge.
- The InstantSend conflict check only runs outside IBD.
- The asset-unlock stable txid zeroes the trailing 132 bytes of the serialization only when the payload is exactly 145 bytes and `payload[0] >= 2`. This is computed before payload validation.

---

#### 2. Script interpreter

**Files:**

| File | Lines |
|---|---|
| `src/script/interpreter.cpp` / `.h` | 1717 / 222 |
| `src/script/script.h` / `.cpp` | 574 / 409 |
| `src/script/standard.{h,cpp}` | — / 236 |
| `src/script/sigcache.cpp` | 104 |
| `src/script/script_error.h` | 80 |
| `src/pubkey.cpp` | — |

**What it does.** This is Bitcoin's pre-SegWit and pre-Taproot interpreter. There is only `SigVersion::BASE`. The sighash is the legacy algorithm. There is no `SIGHASH_FORKID`: the hash types are only `ALL=1, NONE=2, SINGLE=3, ANYONECANPAY=0x80`. `PrecomputedTransactionData` is vestigial.

**Sighash** (`interpreter.cpp:1347-1531`):
- `CTransactionSignatureSerializer` reproduces the legacy serialization, including the Dash 32-bit `version|type` field and **`vExtraPayload` when version ≥ 3 and type ≠ 0**.
- The hash is double-SHA256 over the serialization followed by the 4-byte LE `nHashType`.
- `SIGHASH_SINGLE` with `nIn >= vout.size()` returns `uint256::ONE`.
- In `CheckSig`, `FindAndDelete` of the signature runs in the BASE sigversion.

**Script verification flags** (`interpreter.h`):

| Bit | Flag |
|---|---|
| 0 | P2SH |
| 1 | STRICTENC |
| 2 | DERSIG |
| 3 | LOW_S |
| 4 | NULLDUMMY |
| 5 | SIGPUSHONLY |
| 6 | MINIMALDATA |
| 7 | DISCOURAGE_UPGRADABLE_NOPS |
| 8 | CLEANSTACK |
| 9 | CHECKLOCKTIMEVERIFY |
| 10 | CHECKSEQUENCEVERIFY |
| 14 | NULLFAIL |
| 16 | CONST_SCRIPTCODE |

Bit 15 (`SCRIPT_ENABLE_DIP0020_OPCODES`) was removed in commit `61bc3002b0` ("make opcodes available from genesis block"). There are no WITNESS or TAPROOT flags.

**Consensus flags per block** (`GetBlockScriptFlags`, `validation.cpp:2420`):
- `P2SH` is always set ("always active on Dash chains").
- `DERSIG` if BIP66 is active.
- `CHECKLOCKTIMEVERIFY` if BIP65 is active.
- `CHECKSEQUENCEVERIFY` if CSV is active.
- `NULLDUMMY` if BIP147 is active.
- Nothing else: in blocks, NULLFAIL, LOW_S and the other policy flags are not consensus.

**Policy flags:**
- `MANDATORY` = P2SH | DERSIG | NULLDUMMY | CLTV | CSV.
- `STANDARD` = MANDATORY | STRICTENC | MINIMALDATA | DISCOURAGE_UPGRADABLE_NOPS | CLEANSTACK | NULLFAIL | LOW_S | CONST_SCRIPTCODE.

**Opcodes** (`script.h:93-201`):
- `MAX_OPCODE = OP_CHECKDATASIGVERIFY (0xbb)`.
- Always-disabled opcodes (they fail even in unexecuted branches; `interpreter.cpp:387`): `OP_INVERT 0x83`, `OP_2MUL 0x8d`, `OP_2DIV 0x8e`, `OP_MUL 0x95`, `OP_LSHIFT 0x98`, `OP_RSHIFT 0x99`.
- **DIP0020 opcodes enabled unconditionally:**
  - `OP_CAT 0x7e`: result must be ≤ 520 bytes, else `PUSH_SIZE`.
  - `OP_SPLIT 0x7f` (replaces SUBSTR): the position is a CScriptNum; `<0` or `>len` → `INVALID_SPLIT_RANGE`.
  - `OP_NUM2BIN 0x80` (replaces LEFT): `size = (uint64)CScriptNum(...).getint()`, so a negative size becomes huge and fails with `PUSH_SIZE` when > 520. It minimally encodes the number, fails with `IMPOSSIBLE_ENCODING` if it doesn't fit, and moves the sign bit to the last byte.
  - `OP_BIN2NUM 0x81` (replaces RIGHT): minimally encodes; must then be ≤ 4 bytes (`IsMinimallyEncoded` with the default max size 4), else `INVALID_NUMBER_RANGE`.
  - `OP_AND`, `OP_OR`, `OP_XOR` (0x84–0x86): operands must be the same size (`INVALID_OPERAND_SIZE`).
  - `OP_DIV`, `OP_MOD` (0x96, 0x97): divide-by-zero errors; C++ truncating semantics on int64 `CScriptNum`.
  - `OP_CHECKDATASIG 0xba` and `OP_CHECKDATASIGVERIFY 0xbb`, stack `(sig msg pubkey)`. The message hash is a **single SHA256** of `msg`. The signature passes through **`CheckSignatureEncoding`, the same function as CHECKSIG**: under DERSIG it must be strict DER **plus exactly one trailing byte**, and under STRICTENC that byte must be a defined hash type. `CPubKey::Verify` uses lax DER parsing, which ignores the trailing byte. NULLFAIL applies.
- `OP_NOP1` and `OP_NOP4`..`OP_NOP10` (0xb3–0xb9) remain upgradable NOPs. `0xbc` and above are bad opcodes when executed.

**Limits:**

| Limit | Value |
|---|---|
| `MAX_SCRIPT_ELEMENT_SIZE` | 520 |
| `MAX_OPS_PER_SCRIPT` | 201 (counts opcodes > OP_16, including CHECKDATASIG) |
| `MAX_PUBKEYS_PER_MULTISIG` | 20 |
| `MAX_SCRIPT_SIZE` | 10000 |
| `MAX_STACK_SIZE` | 1000 (stack plus altstack) |
| `CScriptNum` default max size | 4 bytes |

**`VerifyScript`:** Bitcoin legacy behavior: SIGPUSHONLY; P2SH requires a push-only scriptSig; CLEANSTACK requires P2SH.

**Caches** (not consensus, but they matter for performance):
- Signature cache: CuckooCache keyed by salted SHA256 of (sighash ‖ pubkey ‖ sig).
- Script execution cache: SHA256(nonce ‖ txid ‖ flags).
- `DEFAULT_MAX_SIG_CACHE_BYTES` is 32 MiB, split ½ / ½.
- Up to 15 script-check threads.

##### Notes for a Rust reimplementation
- Do not import BCH semantics: Dash's CHECKDATASIG requires a trailing hash-type-like byte under DERSIG, and its sigops are not counted.
- The DIP0020 opcodes are valid at every height, including pre-activation blocks.
- The sighash must include `vExtraPayload`. The `SIGHASH_SINGLE` bug must return 1.
- `OP_NUM2BIN` takes `getint()`, which clamps to int32, and then casts to uint64.

---

#### 3. Proof of work (`src/pow.cpp`, 256 lines; `src/hash_x11.h`, 92; `src/crypto/x11/*`, ~11.7k lines including SIMD variants)

**Block header:** the same 80 bytes as Bitcoin (`nVersion i32, hashPrevBlock, hashMerkleRoot, nTime u32, nBits u32, nNonce u32`). `GetHash()` is **X11** of those 80 bytes, a 512-bit chain in this order:

blake → bmw → groestl → skein → jh → keccak → luffa → cubehash → shavite → simd → echo

Each stage hashes the previous 64-byte output, and the result is the first 32 bytes (`trim256`). The transaction merkle tree and txids remain double-SHA256.

**`GetNextWorkRequired(pindexLast, pblock)`:**
1. If `pindexLast->nHeight < nMinimumDifficultyBlocks` (devnet only) → powLimit.
2. If `height + 1 < nPowKGWHeight` → `GetNextWorkRequiredBTC`. This is the Bitcoin retarget with interval = `nPowTargetTimespan / nPowTargetSpacing` (mainnet 86400/150 = 576), a ×¼..×4 clamp, and the testnet rule: if `fPowAllowMinDifficultyBlocks` and the block time is more than 2×spacing after the previous block → powLimit; otherwise walk back to the last non-min-difficulty block.
3. If `fPowNoRetargeting` (regtest) → powLimit.
4. If `fPowAllowMinDifficultyBlocks` (testnet and devnets):
   - block time > prev + 7200 s → powLimit.
   - block time > prev + 4×spacing (600 s) → `min(prev.nBits target × 10, powLimit)`.
5. If `height + 1 < nPowDGWHeight` → **KimotoGravityWell**:
   - `PastBlocksMin = (uint64)(timespan × 0.025) / spacing`, `PastBlocksMax = timespan × 7 / spacing`.
   - Running average of targets with a signed-safe update.
   - `EventHorizonDeviation = 1 + 0.7084 × pow(mass/28.2, -1.228)` (double precision).
   - Break when the rate ratio is outside [1/EHD, EHD] once mass ≥ min.
   - `new = avg × actual / target`, capped at powLimit.
6. Otherwise → **DarkGravityWave v3**:
   - Needs at least 25 blocks (`pindexLast->nHeight >= 24`), else powLimit.
   - Over 24 blocks: `avg = target(1)`, then `avg = (avg × n + target_n) / (n + 1)` for n = 2..24. This is not a true average.
   - `actual = last.time - 24th_back.time`. It spans 23 intervals, while `nTargetTimespan = 24 × spacing`. **This off-by-one is consensus** (it gives about 2.626 min per block on mainnet).
   - Clamp actual to [T/3, 3T], `new = avg × actual / T`, cap at powLimit.

**`CheckProofOfWork`:** the compact target must not be negative or overflowing, must be nonzero and ≤ powLimit, and the hash must be ≤ target. Dash has no `PermittedDifficultyTransition` / headers-presync.

##### Notes for a Rust reimplementation
- KGW and the mainnet ≤ 68589 50% tolerance check use IEEE doubles and libm `pow`. Replicate them exactly, or embed known-good results for the historical range.
- `arith_uint256` multiply and divide semantics (wrapping) must match.
- DGW reads `pblock` only on min-difficulty networks.
- The X11 implementations need test vectors. SPH reference behavior is canonical; the SIMD dispatch in `crypto/x11/dispatch.cpp` is only an optimization.

---

#### 4. Block assembly and mining

**Files:**

| File | Lines |
|---|---|
| `src/node/miner.cpp` | 698 |
| `src/rpc/mining.cpp` | 1159 |
| `src/masternode/payments.cpp` | 474 |
| `src/governance/superblock.cpp` | 679 |
| `src/evo/cbtx.{h,cpp}` | 89 / 211 |

**`CreateNewBlock`** (`miner.cpp:178-350`):
- Reserves 1000 bytes and 100 sigops for the coinbase.
- `nBlockMaxSize` is clamped to `[1000, MaxBlockSize - 1000]` (default `DEFAULT_BLOCK_MAX_SIZE = 2000000`); max sigops = `MaxBlockSigOps`.
- `nVersion = ComputeBlockVersion` (`-blockversion` override off mainnet).
- Once DIP0003 is active, it first adds **quorum commitment transactions** from `GetMineableCommitmentsTx` for each enabled LLMQ type (fee 0, sigops 0), then mempool packages via `addPackageTxs`.
  - `addPackageTxs` tracks a `CCreditPoolDiff` built with subsidy 0 (withdrawal limits and duplicate unlock indexes) and the EHF signal stage (duplicate MNHF bits).
  - `TestPackageTransactions` requires `IsFinalTx`, re-runs `CheckSpecialTx` (`check_sigs = false`) and the V24 shared-collateral checks, and when InstantSend is enabled requires each non-IS-locked tx with inputs to satisfy `IsTxSafeForMining`: first seen at least 10 minutes ago (`WAIT_FOR_ISLOCK_TIMEOUT`).

**Coinbase:**
- `vout[0] = subsidy + fees`, where the subsidy comes from `GetBlockSubsidyInner(prev.nBits, prev.nHeight, V20)`.
- Before DIP3: scriptSig `<height> OP_0`.
- After DIP3: scriptSig `OP_RETURN` (1 byte), `nVersion = 3`, `nType = 5`, and a **CbTx payload**:

| CbTx field | Present from version |
|---|---|
| `nVersion` (uint16) | 1 |
| `nHeight` (int32) | 1 |
| `merkleRootMNList` | 1 |
| `merkleRootQuorums` | 2 |
| `COMPACTSIZE(bestCLHeightDiff)`, `bestCLSignature` (96-byte BLS), `creditPoolBalance` (int64) | 3 |
| `merkleRootAssetUnlocks` | 4 |

- Version selection: V24 && V20 → 4; V20 → 3; DIP0008 → 2; otherwise 1.
- `bestCL` selection (`CalcCbTxBestChainlock`): ChainLocks below V19 height are ignored. The block must carry a ChainLock at least as new as the previous coinbase's (`diff = prevDiff + 1`) if it has nothing better.
- `creditPoolBalance = GetCreditPoolDiffForBlock(...).GetTotalLocked()`.

**`FillBlockPayments`** appends masternode outputs (subtracted from `vout[0]`) and then superblock outputs (added on top).

**Masternode payment** (`payments.cpp:56-112`, `GetMasternodePayment(height, blockValue = subsidy + fees)`):
1. Start at `blockValue/5`.
2. Add `blockValue/20` when the height is above I, I+P, and I+2P; add `blockValue/40` when above I+3P … I+7P and I+9P. Here I = `nMasternodePaymentsIncreaseBlock` and P = `nMasternodePaymentsIncreasePeriod` (note: no step at I+8P).
3. If `height < BRRHeight` → return.
4. `reallocStart = BRRHeight - BRRHeight % cycle + cycle`; if `height < reallocStart` → return.
5. If the era is not Classic → `blockValue*3/4`.
6. Otherwise `blockValue * period[min((h - start)/(3*cycle), 18)] / 1000`, with periods {513, 526, 533, 540, 546, 552, 557, 562, 567, 572, 577, 582, 585, 588, 591, 594, 597, 599, 600}.

**Splitting the payment** (`GetBlockTxOuts`):
- In the EvoReward era: `platformReward = MNpay(subsidy only) * 375 / 1000`, paid as an `OP_RETURN` output and subtracted from the masternode reward.
- Operator: `reward * nOperatorReward / 10000` if an operator payout script is set.
- Owner payouts (multi-payout: the last payout gets the remainder), or V24 shared-collateral shares.
- If the masternode list is empty, only the platform output is expected.

**Payee validation:**
- `IsTransactionValid` is skipped below `DIP0003EnforcementHeight`.
- The expected outputs must appear in the coinbase as an unordered multiset when `strict_multiplicity` (V24), and as a set otherwise.
- **If the payee cannot be computed, it returns true.**

**`IsBlockValueValid`:**
- Below `nBudgetPaymentsStartBlock`: coinbase out ≤ reward.
- Up to `nSuperblockStartBlock`: old budget-window logic.
- At superblock heights (`h % nSuperblockCycle == 0`, h ≥ start): coinbase out ≤ reward + `GetPaymentsLimit`. The limit is `GetSuperblockSubsidyInner(nBits, h-1, V20) × cycle`, with `nBits` = powLimit compact on min-difficulty networks and `1` otherwise (i.e. the minimum subsidy). It also requires `IsValidSuperblock` if the superblock was triggered and validation is required.

**GBT** (`rpc/mining.cpp:593-1040`):
- Refuses when there are no peers, during IBD, or when masternode sync is incomplete and the next height is a superblock height.
- Standard fields: `capabilities` `["proposal"]`, `version`, `rules` (always contains `"csv"`, plus active BIP9 names), `vbavailable`, `vbrequired: 0`, `previousblockhash`, `transactions[{data, hash, depends, fee, sigops}]` (includes quorum commitment txs; excludes the coinbase), `coinbaseaux {}`, `coinbasevalue` (= the full coinbase `GetValueOut`), `longpollid`, `target`, `mintime = MTP + 1`, `mutable`, `noncerange`, `sigoplimit`, `sizelimit`, `curtime`, `bits`, `previousbits`, `height`.
- Dash fields:
  - `masternode`: array of `{payee, script, amount}`
  - `masternode_payments_started`
  - `masternode_payments_enforced: true`
  - `superblock`: array of `{payee, script, amount}`
  - `superblocks_started`
  - `superblocks_enabled: true`
  - `coinbase_payload`: hex of the CbTx `vExtraPayload`
- External miners must build `nVersion = 3`, `nType = 5` coinbases with that payload.
- Other RPCs: `generatetoaddress`, `generatetodescriptor`, `generateblock`, `submitblock`, `prioritisetransaction`, `getprioritisedtransactions`, `getmininginfo`, `getnetworkhashps` (`generate` has been removed).

##### Notes for a Rust reimplementation
- The masternode payment uses subsidy + fees, but the platform share uses the subsidy alone.
- Coinbase output matching is by exact `(value, script)` equality.
- The superblock limit's `nBits = 1` trick yields the minimum subsidy on mainnet.

---

#### 5. Mempool and fee policy

**Files:**

| File | Lines |
|---|---|
| `src/txmempool.cpp` / `.h` | 2126 / 915 |
| `src/policy/policy.{h,cpp}` | 118 / 215 |
| `src/policy/fees.{h,cpp}` | 310 / 1051 |
| `src/txorphanage.cpp` | 271 |
| `src/kernel/mempool_persist.cpp` | 199 |
| `MemPoolAccept` in `src/validation.cpp` | ~520–1760 |

**There is no RBF of any kind:** a conflict returns `txn-mempool-conflict` ("RBF doesn't exist in Dash", `validation.cpp:913`).

**Dash steps in `PreChecks`:**
- `ContextualCheckTransaction` at the tip.
- Quorum commitments are never accepted (`qc-not-allowed`).
- `IsStandardSpecialTx`: asset locks with at most 100 inputs and total size ≤ 20480, and asset-lock payload version ≥ 2 is nonstandard.
- An ISLOCK conflict → `tx-txlock-conflict` (`TX_CONFLICT_LOCK`) and a double-spend notification.
- If an ISLOCK is waiting for this tx, conflicting mempool txs (including ProTx and asset-unlock conflicts) are evicted instead of rejecting the new tx.
- Asset unlock: reject if the index is already in the credit pool (`txn-already-known`) or if a mempool instance with requested height ≥ this one exists (`assetunlock-stale-instance`). Re-signed instances can replace older ones (`TryAssetUnlockRefresh` / `ReplaceAssetUnlockInstance`).
- V24 shared-collateral checks.
- **MNHF signal txs are exempt from the minimum relay fee and fee-rate checks.**
- `CheckSpecialTxForMempool` (for stable-txid asset unlocks it may validate at the requested height).
- `protx-dup` conflict maps: `mapProTxRefs`, `mapProTxAddresses`, `mapProTxPubKeyIDs`, `mapProTxBlsPubKeyHashes`, `mapProTxPlatformNodeIDs`, `mapProTxCollaterals`, plus a cross-BLS-scheme check.
- The mempool also keeps `mapAddress` and `mapSpent` (mempool address and spent indexes), `mapAssetUnlockExpiry`, `m_asset_unlock_instances`, `m_asset_unlock_indexes`, and `m_pending_asset_unlock_amount` (arith_uint256).

**Size accounting:** vsize = `max(size, sigops × 20)` (`DEFAULT_BYTES_PER_SIGOP 20`).

**Policy defaults:**

| Setting | Value |
|---|---|
| `minrelaytxfee` / incremental relay fee | 1000 duff/kB |
| Dust relay fee | 3000/kB → 546 duffs for P2PKH; dust size = output size + 148 |
| `blockmintxfee` | 1000 |
| `MAX_STANDARD_TX_SIZE` | 100000 (the tx must be **smaller** than this to be standard) |
| `MIN_STANDARD_TX_SIZE` | 65 |
| `MAX_P2SH_SIGOPS` | 15 |
| `MAX_STANDARD_TX_SIGOPS` | 4000 |
| `MAX_STANDARD_SCRIPTSIG_SIZE` | 1650 (must also be push-only) |
| Standard tx version | 1..3 |
| Bare multisig | permitted, up to x-of-3 |
| Datacarrier | on; 83 bytes; one `OP_RETURN` output |
| Ancestor / descendant limits | 25 / 101 kvB each; CPFP carve-out +10000 |
| `maxmempool` | 300 MB |
| `mempoolexpiry` | 336 h |
| Orphan pool | `DEFAULT_MAX_ORPHAN_TRANSACTIONS_SIZE = 10` MB |

Standard output types: NONSTANDARD, PUBKEY, PUBKEYHASH, SCRIPTHASH, MULTISIG, NULL_DATA. The shared-collateral template script (`OP_DROP`/`OP_TRUE`) is nonstandard except as the declared collateral output of a shared ProRegTx; it may be spent with an empty scriptSig by a ProDisTx.

**Persistence:**
- **`mempool.dat`**: `uint64 version = 1`, `uint64 count`, then for each tx `{CTransaction, int64 time, int64 feeDelta}`, then `map<uint256, CAmount> mapDeltas`, then `set<uint256> unbroadcast`. **There is no XOR key**; Dash did not backport Bitcoin's v2 format.
- **`fee_estimates.dat`**: int32 140100, int32 `CLIENT_VERSION`, int32 bestSeenHeight, int32 first and best of the historical span, encoded-double vector of buckets, then three `TxConfirmStats` (decay, scale, averages). Stale after 60 h. Bucket range 1000..1e7, spacing 1.05; decays .962 / .9952 / .99931.

##### Notes for a Rust reimplementation
- Mempool behavior is policy, but consensus-adjacent pieces matter: ISLOCK conflict handling and miner tx safety (the 10-minute rule) affect what ChainLocks will sign.

---

#### 6. Indexes (`src/index/`, 1132 header lines in total)

All indexes are `BaseIndex` subclasses (`index/base.cpp`, 454 lines). Each has its own LevelDB under `<datadir>/indexes/<name>/`, **not obfuscated**, with key `'B'` = `CBlockLocator` (client-version VARINT + vector<hash>) recording the index's best block. They sync asynchronously in the background.

| Index | Flag (default) | Directory | Keys and values | Prune allowed |
|---|---|---|---|---|
| txindex (`txindex.cpp`, 111) | `-txindex` (**default 1** in Dash; required for masternodes and for governance validation outside regtest) | `indexes/txindex` | `'t'` + txid → `CDiskTxPos` {VARINT nFile (signed), VARINT nPos, VARINT nTxOffset} | no |
| blockfilter (`blockfilterindex.cpp`, 555; `blockfilter.cpp`, 262) | `-blockfilterindex` (default `"0"`; auto `basic` when `-masternodeblsprivkey` is set) | `indexes/blockfilter/basic/` + `fltrNNNNN.dat` (16 MiB max, 1 MiB chunks; record = block hash ‖ encoded filter) | `'t'` + BE uint32 height → (hash, DBVal{hash, header, FlatFilePos}); `'s'` + hash → DBVal; `'P'` → next FlatFilePos; **`'V'` → int version, `CURRENT_VERSION = 2`** (Dash) | yes |
| coinstats (`coinstatsindex.cpp`, 505) | `-coinstatsindex` (0) | `indexes/coinstats` | `'t'` height, `'s'` hash, `'M'` MuHash3072 | yes |
| addressindex (`addressindex.cpp`, 417) | `-addressindex` (0) | `indexes/addressindex` | `'a'` + `CAddressIndexKey` → int64 amount (negative = spend); `'u'` + `CAddressUnspentKey` → `CAddressUnspentValue{int64 amount, CScript, int32 height}` | no |
| spentindex (`spentindex.cpp`, 169) | `-spentindex` (0) | `indexes/spentindex` | `'p'` + `CSpentIndexKey{txid, u32 n LE}` → `CSpentIndexValue{spending txid, u32 vin index, i32 height, i64 amount, u8 addrType, uint160}` | no |
| timestampindex (`timestampindex.cpp`, 96) | `-timestampindex` (0) | `indexes/timestampindex` | `'s'` + `CTimestampIndexKey{BE u32 nTime, block hash}` → bool true | yes |

**BIP158 basic filter:** P = 19, M = 784931, SipHash keys from the block hash. Elements:
- every output script that is non-empty and doesn't start with `OP_RETURN`;
- every spent prevout script (from undo data);
- **Dash extras** (`evo/specialtx_filter.cpp`, 171 lines):
  - ProRegTx: serialized collateral outpoint, owner and voting keyIDs, owner payout scripts, share refund and reward scripts, share owner keyIDs.
  - ProUpServTx: proTxHash, operator payout script.
  - ProUpRegTx: proTxHash, voting keyID, payout scripts.
  - ProUpRevTx and ProDisTx: proTxHash.
  - ProUpShareTx: proTxHash, reward script.
  - ProUpSharedRegTx: proTxHash, voting keyID.
  - AssetLock: non-`OP_RETURN` credit output scripts.

Filter hash = dSHA256(encoded filter); header = dSHA256(filter hash ‖ previous header).

**Address and spent index key formats** (`addressindex_types.h`). The fixed-width keys use **big-endian height and position** so they sort correctly:
- `CAddressIndexKey` (66 bytes): `u8 type` (1 = P2PK/P2PKH, 2 = P2SH), `uint160`, `BE i32 height`, `BE u32 tx position in block` (coinbase = 0), `txid`, `LE u32 vin/vout index`, `u8 spent`.
- `CAddressUnspentKey` (57 bytes): type, uint160, txid, LE u32 index.
- Address extraction: P2SH → script bytes 2..22; P2PKH → bytes 3..23; P2PK → Hash160(pubkey); otherwise the output is not indexed. The spent index still records type 0 with a null hash.
- Both indexes read undo data, so they cannot run on pruned nodes.

**Legacy layout and migration** (`txdb.cpp:480-645`, `MigrateOldIndexData`): older nodes stored these indexes inside `blocks/index` under `'a'`, `'u'`, `'p'`, `'s'` with the same key and value encodings, plus flag `'F'` + "addressindex", "spentindex" or "timestampindex". At startup, entries are copied into the new databases only if the old flag was `'1'`; otherwise stale entries are discarded. Old entries are erased, with a compaction every 256 MiB. The new DB's best block is set to the chainstate `'B'` hash.

**Reindex and prune constraints:**
- `-prune` is incompatible with txindex, addressindex and spentindex. It also forces `-txindex = 0` via a soft-set, and governance requires txindex.
- `-reindex-chainstate` is incompatible with coinstatsindex, blockfilterindex and txindex.
- Toggling these indexes on simply starts a background sync from the genesis block. It needs block and undo data, so no reindex is needed unless the node is pruned.

---

#### 7. Deployments (`src/consensus/params.h`, 241; `src/deploymentinfo.cpp`, 56; `src/versionbits.{h,cpp}`, 142 / 326; `src/deploymentstatus.{h,cpp}`, 71 / 32)

**Buried deployments** (`enum BuriedDeployment : int16_t`, starting at `INT16_MIN`). All are "active at height ≥ param":

| Enum | Name | Height parameter |
|---|---|---|
| HEIGHTINCB | bip34 | `BIP34Height` (also `BIP34Hash`) |
| DERSIG | bip66 | `BIP66Height` |
| CLTV | bip65 | `BIP65Height` |
| BIP147 | bip147 | `BIP147Height` |
| CSV | csv | `CSVHeight` |
| DIP0001 | dip0001 | `DIP0001Height` |
| DIP0003 | dip0003 | `DIP0003Height` (enforcement is separate: `DIP0003EnforcementHeight`/`Hash`, via `DeploymentDIP0003Enforced`) |
| DIP0008 | dip0008 | `DIP0008Height` |
| DIP0020 | dip0020 | `DIP0020Height` (also gates DIP0021 / LLMQ_100_67) |
| DIP0024 | dip0024 | `DIP0024Height` (plus `DIP0024QuorumsHeight`) |
| BRR | realloc | `BRRHeight` |
| V19 | v19 | `V19Height` |
| V20 | v20 | `V20Height` |
| MN_RR | mn_rr | `MN_RRHeight` |
| WITHDRAWALS | withdrawals | `WithdrawalsHeight` |

`DeploymentActiveAfter(prev)` means `prev.height + 1 >= H`. `DeploymentActiveAt(index)` means `index.height >= H`.

**BIP9 deployments** (`DeploymentPos`):
- `DEPLOYMENT_TESTDUMMY` ("testdummy", gbt_force)
- `DEPLOYMENT_V24` ("v24", gbt_force; doubles the withdrawal limit, extended addresses, shared collateral, etc.)

The `BIP9Deployment` fields are: `bit` (default 28), `nStartTime`, `nTimeout`, `min_activation_height`, `nWindowSize` (0 → `nMinerConfirmationWindow`), `nThresholdStart` (0 → `nRuleChangeActivationThreshold`), `nThresholdMin`, `nFalloffCoeff`, `useEHF`. Special start values: `ALWAYS_ACTIVE = -1`, `NEVER_ACTIVE = -2`; `NO_TIMEOUT = INT64_MAX`.

**State machine** (`versionbits.cpp:33-140`). It is Bitcoin's BIP9 with min_activation_height, plus Dash changes:
- **Period:** `nWindowSize ? nWindowSize : nMinerConfirmationWindow`.
- **Dynamic threshold:** `Threshold(attempt) = max(nThresholdMin, nThresholdStart - attempt*attempt*Period/100/nFalloffCoeff)`, in integer math, left-associative. If `nThresholdStart == 0`, the default threshold is used; if `nThresholdMin == 0` or `nFalloffCoeff == 0`, `nThresholdStart` is used. `attempt = (pindexPrev.height - nStartHeight) / Period`, where `nStartHeight` is the first height in STARTED. That height is recovered from the cache by `calculateStartHeight`, walking back through cached STARTED periods.
- **EHF gating** (`useEHF`): `SignalHeight(pindexPrev)` returns the mined height of the MNHF signal for `bit` (from `CMNHFManager::GetSignalsStage`, evodb keys `mnhf_s` / `mnhf_s2`), `INT_MAX` if there is no signal, or 0 if `useEHF` is false. DEFINED→STARTED additionally requires `pindexPrev.height >= SignalHeight` (alongside MTP ≥ start).
  - Signals exist only once V20 is active. A signal is ignored ("expired") if the signal block's MTP is before the deployment's `nStartTime` (bit reuse).
  - A signal is valid in a block only if `IsValidMNActivation(bit, MTP)` holds: within [start, timeout] and `useEHF`.
  - A duplicate bit → `bad-mnhf-duplicate`.
- **Optimization that affects results:** period boundaries below `MinBIP9WarningHeight`, or with MTP < start, or below the signal height are cached as DEFINED.
- `Condition` = `(nVersion & 0xE0000000) == 0x20000000 && (nVersion & (1 << bit))`.
- `ComputeBlockVersion` = `0x20000000` OR the bits of deployments in STARTED or LOCKED_IN. `VERSIONBITS_NUM_BITS = 29`.
- The unknown-bit warning checker is non-consensus.

Per-network values (mainnet V24 shown only to illustrate the fields): bit 13, window 4032, start 3226, min 2420, falloff 5, useEHF true, start time `NEVER_ACTIVE`; testnet start 2026-09-24. Mainnet `nRuleChangeActivationThreshold` = 1815 of 2016.

##### Notes for a Rust reimplementation
- The versionbits result depends on evodb MNHF state, so versionbits evaluation needs access to the EHF signal map as of `pindexPrev`.
- Replicate the cache semantics exactly: the state is evaluated at period-end ancestors (`pindexPrev.height - (pindexPrev.height + 1) % Period`).

---

#### 8. Block storage (`src/node/blockstorage.{cpp,h}`, 963 / 258; `src/flatfile.cpp`, 103; `src/dbwrapper.cpp`, 276)

**Files:** `<datadir>/blocks/blkNNNNN.dat` and `revNNNNN.dat` (5-digit zero-padded).
- `MAX_BLOCKFILE_SIZE` 128 MiB; pre-allocation in 16 MiB chunks for blk and 1 MiB for rev; `-tinyblk` uses 64 KiB, for testing.

**Records:**
- Block record: `pchMessageStart[4]` (network magic; mainnet `bf 0c 6b bd`), then `u32 LE size`, then the serialized block. `BLOCK_SERIALIZATION_HEADER_SIZE = 8`.
- Undo record: magic, `u32 size`, `CBlockUndo`, then a **32-byte checksum = dSHA256(prev block hash ‖ CBlockUndo)**. Reserve is size + 40.

**There is no XOR obfuscation of block files.** Grepping for `blocksxor`, `xor.dat` and `XOR_KEY` finds nothing. The `AutoFile` XOR plumbing exists in `streams.h` but block files don't use it. LevelDB value obfuscation applies only to `chainstate/`.

**Pruning:**
- `MIN_BLOCKS_TO_KEEP = 288`; `MIN_DISK_SPACE_FOR_BLOCK_FILES` = 945 MiB; `PRUNE_LOCK_BUFFER = 10`.
- The `prunedblockfiles` flag is stored in the block-tree DB.

**Import:**
- `-reindex` wipes the block index and chainstate and re-imports `blk*.dat` with `LoadExternalBlockFile` (`validation.cpp:5422`). That function scans for the magic byte by byte, requires `80 <= size <= MaxBlockSize()` (2 MB), and buffers `2 × MaxBlockSize`. It keeps a map of blocks whose parent is unknown and sets the `'R'` flag while running.
- `-loadblock=<file>`: external files in the same framing.
- There is no `bootstrap.dat` handling.
- `-reindex-chainstate` replays from the existing block files.
- `ThreadImport` then runs `ActivateBestChain` on every chainstate and loads the mempool.

##### Notes for a Rust reimplementation
- File positions in `CDiskBlockIndex` point **after** the 8-byte header.
- Undo positions require the parent hash for the checksum.
- Keep the magic per network: the blk files of mainnet, testnet, devnet and regtest are not interchangeable.

---

##### Cross-cutting pitfalls

1. **Floating point in consensus.** Subsidy, KGW, and the early DGW tolerance all use doubles and libm `pow`.
2. **"Previous height" conventions.** The subsidy and `GetPaymentsLimit` use `h-1` and prev bits.
3. **State-dependent block validity.** Masternode payee, superblocks, ChainLock conflicts and ISLOCK conflicts depend on governance data, sync state, ChainLocks and IS state. Mark such failures "do-not-punish", allow reconsideration, and treat ChainLocks as final.
4. **Evodb and chainstate must stay consistent.** Evodb's best-block marker is checked on every connect and disconnect.
5. **Txid computation is not simply hash-of-serialization** for version-2+ asset unlocks.
6. **BIP30 and DIP0020 are effectively no-op gates.** Script flags are only {P2SH always, DERSIG, CLTV, CSV, NULLDUMMY} by height.

### A.2 P2P networking: net, net_processing, protocol, transport, addrman, banman

*Section numbers below (§1, §2, …) are local to A.2.*

**Source:** `/work/home/agent/workspace/dash`, branch `develop`, HEAD `15db478750`. All `src/...` paths below are relative to that root. The tree reports client version 24.1.0 (not a release build, per `configure.ac:3-6`) and `PROTOCOL_VERSION = 70242`.

**Biggest fingerprinting risks, up front:**
- **Messages Dash doesn't have.** No `feefilter`, `wtxidrelay` or `reject` message exists. BIP324 short ID 5 (feefilter) maps to an empty name.
- **Extra `version` fields.** Dash appends a 32-byte mnauth challenge and a masternode flag byte after `fRelay`.
- **Different defaults.** `NODE_BLOOM` is on by default, and `NODE_HEADERS_COMPRESSED` is always set.
- **Chatter after sync.** A dashd automatically sends `getsporks`, `mempool` and `govsync` once the handshake and sync start.
- **Score-based punishment is still there.** Misbehavior points accumulate toward a threshold of 100; Dash did not switch to upstream's newer discourage-only model.

#### 0. Approximate file sizes (wc -l)

| File | Lines |
|---|---|
| src/net.cpp / net.h | 5058 / 2072 |
| src/net_processing.cpp / .h | 7000 / 239 |
| src/protocol.cpp / .h | 373 / 572 |
| src/addrman.cpp / addrman.h / addrman_impl.h | 1383 / 226 / 295 |
| src/addrdb.cpp | 251 |
| src/banman.cpp / .h | 215 / 97 |
| src/netbase.cpp / .h | 840 / 348 |
| src/i2p.cpp | 492 |
| src/torcontrol.cpp | 713 |
| src/mapport.cpp | 336 |
| src/bip324.cpp | 109 |
| src/net_permissions.cpp | 124 |
| src/netfulfilledman.cpp | 97 |
| src/node/eviction.cpp | 240 |
| src/evo/mnauth.cpp | 210 |
| src/txrequest.cpp | 762 |
| src/node/txreconciliation.cpp | 169 |
| src/version.h | 81 |

---

#### 1. Protocol constants

##### 1.1 Protocol versions (`src/version.h:14-77`)

| Constant | Value | Meaning |
|---|---|---|
| PROTOCOL_VERSION | 70242 | Current version |
| INIT_PROTO_VERSION | 209 | Serialization version used before the handshake. The v1 header and the `version` message are written with it. |
| MIN_PEER_PROTO_VERSION | 70221 | Peers below this are disconnected (`net_processing.cpp:3964`) |
| MIN_MASTERNODE_PROTO_VERSION | 70242 | Minimum version for a masternode to take part in DKG |
| ADDRV2_PROTO_VERSION | 70223 | `sendaddrv2` is only sent if the common version ≥ this. Below it, `sendaddrv2` is ignored. |
| BLS_SCHEME_PROTO_VERSION | 70225 | Basic vs legacy BLS serialization |
| DMN_TYPE_PROTO_VERSION | 70227 | Masternode type field in the simplified masternode list (SML) |
| SMNLE_VERSIONED_PROTO_VERSION | 70228 | Versioned SML entries |
| MNLISTDIFF_VERSION_ORDER | 70229 | |
| MNLISTDIFF_CHAINLOCKS_PROTO_VERSION | 70230 | |
| NO_LEGACY_ISLOCK_PROTO_VERSION | 70231 | |
| DSQ_INV_VERSION | 70234 | `senddsq` means "inv" mode at or above this, "push everything" below it |
| INCREASE_MAX_HEADERS2_VERSION | 70235 | `headers2` limit raised from 2000 to 8000. Also gates `sendtxrcncl`. |
| BIP324_DASH_BASELINE_VERSION | 70235 | v2 short IDs are only used at or above this version |
| EFFICIENT_QRINFO_VERSION | 70236 | `qrinfo` built the new way at or above this |
| ISDLOCK_CYCLEHASH_UPDATE_VERSION | 70237 | Changes what `cycleHash` means; the wire format is the same |
| PLATFORM_BAN_VERSION | 70238 | |
| QFCOMMIT_STALE_REPROP_BAN_VERSION | 70239 | |
| PLATFORMBAN_V2_SHORT_ID_VERSION | 70240 | v2 short ID 168 for `platformban` |
| COINJOIN_REBALANCE_VERSION | 70241 | `dsa` gains an `nFlags` field (`coinjoin/coinjoin.h:162-164`) |
| ASSET_UNLOCK_INV_VERSION | 70242 | MSG_ASSET_UNLOCK inventory type |

##### 1.2 Service bits (`src/protocol.h:311-340`)

| Flag | Bit | Notes |
|---|---|---|
| NODE_NETWORK | 1<<0 | Set when not pruning (`init.cpp:2354`) |
| NODE_BLOOM | 1<<2 | **On by default**: `DEFAULT_PEERBLOOMFILTERS = true` (`net_processing.h:44`; Bitcoin defaults to false). Required for masternodes (`init.cpp:1402`). |
| NODE_COMPACT_FILTERS | 1<<6 | Needs `-peerblockfilters` (default false, `net_processing.h:45`) plus `-blockfilterindex=basic`. **Forced on for masternodes** (`init.cpp:1099-1103`, `1417`). |
| NODE_NETWORK_LIMITED | 1<<10 | Always set (`init.cpp:1125`) |
| NODE_HEADERS_COMPRESSED | 1<<11 | Dash-only (DIP-25). Always set (`init.cpp:1125`). |
| NODE_P2P_V2 | 1<<12 | `-v2transport`, default **true** (`net.h:142`, `init.cpp:1235`) |

**Default local services**, non-pruned:
- Regular node: NETWORK | BLOOM | NETWORK_LIMITED | HEADERS_COMPRESSED | P2P_V2 = **0x1C05**.
- Masternode: adds COMPACT_FILTERS = **0x1C45**.

Peers that connect with the `bloomfilter` whitelist permission get NODE_BLOOM even if it is disabled (`net.cpp:1975-1978`).

**Desirable services** (`protocol.cpp:252-257`): NODE_NETWORK, or NODE_NETWORK_LIMITED once out of initial block download (IBD). DNS seeds are queried as `x1.<seed>` (`net.cpp:2763`).

##### 1.3 Chain parameters (`src/chainparams.cpp`, `src/chainparamsbase.cpp:42-48`)

| Chain | Magic (pchMessageStart) | P2P port | RPC port | Onion target | Platform P2P / HTTP | DNS seeds |
|---|---|---|---|---|---|---|
| main | bf 0c 6b bd | 9999 | 9998 | 9996 | 26656 / 443 | `dnsseed.dash.org.` |
| testnet | ce e2 ca ff | 19999 | 19998 | 19996 | 22000 / 22001 | `testnet-seed.dashdot.io.` (supports x9 only) |
| devnet | e2 ca ff ce | 19799 | 19798 | 19796 | 22100 / 22101 | none |
| regtest | fc c1 b7 dc | 19899 | 19898 | 19896 | 22200 / 22201 | `dummySeed.invalid.` |

- Fixed seeds: `chainparams_seed_main` and `chainparams_seed_test` in `chainparamsseeds.h`, BIP155-encoded and converted by `ConvertSeeds` (`net.cpp:207`).
- I2P uses port 0 (`I2P_SAM31_PORT`, `netaddress.h:113`).
- `nLLMQConnectionRetryTimeout` is 60 s (1 s on regtest).

---

#### 2. P2P message table

All type names are listed in `protocol.cpp:19-93` and `allNetMessageTypes` (`protocol.cpp:99-176`). A type not on this list is logged as `Unknown command` and ignored, with no penalty (`net_processing.cpp:5823`).

Routing:
- The main handler is `PeerManagerImpl::ProcessMessage` (`net_processing.cpp:3914`).
- Known types it doesn't handle directly fall through to the "extension" chain at `net_processing.cpp:5768-5821`:
  1. `m_cj_walletman->processMessage`
  2. `CMNAuth::ProcessMessage`
  3. `quorum_block_processor->ProcessMessage`
  4. `ProcessPlatformBanMessage`
  5. inline CLSIG handling
  6. every registered `NetHandler::ProcessMessage` (registered at `init.cpp:2208-2281`)

"BRO-violating" below means the type is in `netMessageTypesViolateBlocksOnly` (`protocol.cpp:185-212`). Receiving such a message on a block-relay-only, feeler or `-blocksonly` connection causes a **disconnect** (`net_processing.cpp:4377-4381`).

##### 2.1 Bitcoin-heritage messages

| Command | Direction | Payload | Handler (file:line) | Gating / notes |
|---|---|---|---|---|
| version | both | int32 nVersion, uint64 services, int64 time, addrYou (uint64 svc + 16-byte IP + port), addrMe (same layout, **Dash sends our services and an empty CService**), uint64 nonce, var_str subver (≤256), int32 start_height, bool relay, **uint256 mnauthChallenge, bool fMasternode** | ProcessMessage `:3931`; sent by `PushNodeVersion` `:1560-1592` | Written with INIT_PROTO_VERSION. Outbound sends it on connect; inbound sends it only after receiving one. Rejected if the version is below 70221, or the peer lacks desirable services on a service-expecting connection. An inbound peer with `fMasternode=1` is disconnected if we are not a masternode. On devnet, the subver must contain `devnet.<name>`. |
| verack | both | empty | `:4183` | After it: MNAUTH (masternodes only), SENDHEADERS2/SENDHEADERS, SENDCMPCT, SENDDSQUEUE, QWATCH (see §6.1) |
| sendaddrv2 | both | empty | `:4288` | Only between VERSION and VERACK; afterwards it means disconnect. Ignored if the common version < 70223. |
| addr / addrv2 | both | vector<CAddress> (v1, or BIP155 with ADDRV2_FORMAT) | `:4384` | More than 1000 entries costs 20 points. Token bucket of 0.1 addr/s, capped at 1000. Stored with a 2 h time penalty. Relayed only if the message has ≤10 addresses, each <10 min old, and no getaddr is pending. |
| inv | both | vector<CInv> (uint32 type + uint256) | `:4503` | More than 50000 entries costs 20 points. Unknown types are skipped. Non-block invs are dropped during IBD except MSG_SPORK. An ISDLOCK inv on a block-relay-only connection from a peer with common version > 70223 means disconnect. |
| getdata | both | vector<CInv> | `:4609` → `ProcessGetData` `:2931` | More than 50000 entries costs 20 points. Answered with `notfound` for anything missing. |
| notfound | both | vector<CInv> | `:5740` | Limit is min(50000, MAX_PEER_OBJECT_ANNOUNCEMENTS+16); exceeding it costs 20 points |
| getblocks | both | CBlockLocator + stop hash | `:4632` | Standard |
| getheaders | both | CBlockLocator (int32 version + vector<uint256>) + stop hash | `:4756` | Locator >101 entries means disconnect. Below minimum chain work (and no `download` permission) we send an empty `headers`/`headers2`. Up to 2000 results. |
| headers | both | vector<CBlockHeader + varint 0> | `:5305` | More than 2000 costs 20 points |
| tx | both | CTransaction (Dash special-transaction format: nVersion/nType 16-bit pair, vExtraPayload) | `:4857` | Ignored during IBD. BRO-violating. |
| block | both | CBlock | `:5344` | |
| merkleblock | out | CMerkleBlock | `ProcessGetBlockData` `:2841` | **Dash also pushes `isdlock` for every matched tx that has an InstantSend lock** (`:2852-2857`) |
| getaddr | in | empty | `:5385` | Ignored on outbound connections. Answered once per connection, from a 21–27 h per-network cache (`net.cpp:4304`). Up to 1000 addresses, max 23%. |
| mempool | both | empty | `:5422` | Needs NODE_BLOOM offered or the `mempool` permission, otherwise disconnect. Answered on the next trickle with tx invs, matching ISDLOCK invs, and the best CLSIG inv (`:6612-6652`). |
| ping / pong | both | uint64 nonce | `:5452` / `:5470` | |
| filterload | in | CBloomFilter (vData ≤36000 bytes, nHashFuncs ≤50, nTweak, nFlags) | `:5525` | Without NODE_BLOOM: disconnect. Oversized or malformed: 100 points. |
| filteradd | in | vector ≤520 bytes | `:5556` | 100 points if there is no filter or the data is bad |
| filterclear | in | empty | `:5588` | |
| sendheaders | both | empty | `:4257` | Sent only to peers **without** NODE_HEADERS_COMPRESSED |
| sendcmpct | both | bool hb + uint64 version (=1) | `:4269` | Only version 1 (txid-based, no witness). Sent with (0,1) only when `CanRelay()`. |
| cmpctblock / getblocktxn / blocktxn | both | BIP152 | `:5048` / `:4700` / `:5291` | MAX_CMPCTBLOCK_DEPTH 5, MAX_BLOCKTXN_DEPTH 10, 3 compact blocks in flight per block |
| getcfilters / getcfheaders / getcfcheckpt | in | BIP157 | `:5634-5647` | ≤1000 / ≤2000 / checkpoint interval 1000. Requires NODE_COMPACT_FILTERS. |
| cfilter / cfheaders / cfcheckpt | out | BIP157 | | Not processed on receipt (treated as unknown or ignored) |
| sendtxrcncl | both | uint32 version (=1) + uint64 salt | `:4306`; sent at `:4086-4100` | Erlay. **`-txreconciliation` defaults to false** (`node/txreconciliation.h:15`), so it is never sent by default. When enabled, it requires common version ≥70235. |

**Not present anywhere:** `feefilter`, `wtxidrelay`, `reject`, `alert`, `getaddrv2`. `feefilter` is explicitly unimplemented (`net.cpp:898`).

##### 2.2 Dash-specific messages

| Command | Payload class (key fields) | Handler | Gating / notes |
|---|---|---|---|
| spork | `CSporkMessage` {int32 nSporkID, int64 nValue, int64 nTimeSigned, vchSig} (`spork.h:115`) | net_processing `:5685` | Malformed or invalid signature: 100 points. Relayed by inv MSG_SPORK. |
| getsporks | empty | `:5709` | Replies with every active spork. A repeat is ignored only while the active set hasn't changed. **Sent automatically by `SyncManager` to every relay-capable peer** (`node/sync_manager.cpp:206-210`). |
| senddsq | bool | `:4481` | false means NONE. true means INV if common version ≥70234, else ALL (full `dsq` push). Sent by us on verack unless `RejectIncomingTxs` (`:4219-4221`). |
| dsa | `CCoinJoinAccept` {int nDenom, CMutableTransaction txCollateral, [nFlags if version ≥70241]} | CCoinJoinServer (masternode only) `coinjoin/server.cpp:56` | BRO-violating |
| dsi | `CCoinJoinEntry` {vecTxDSIn, txCollateral, vecTxOut} | CJ server `:60` | BRO-violating |
| dsf | int32 sessionID + CMutableTransaction | CJ client `coinjoin/client.cpp:110` | BRO-violating |
| dss | vector<CTxIn> | CJ server `:62` | BRO-violating |
| dsc | sessionID + messageID | CJ client `:124` | BRO-violating |
| dssu | `CCoinJoinStatusUpdate` {nSessionID, nState, nStatusUpdate, nMessageID} | CJ client `:105` | BRO-violating |
| dstx | `CCoinJoinBroadcastTx` {tx, m_protxHash, vchSig, sigTime} | net_processing `:4857` (shared with `tx`) | BRO-violating. Inv type MSG_DSTX. Announced as MSG_TX to peers below 70241 when the tx is an unbalanced (rebalance) DSTX (`:3608-3612`). |
| dsq | `CCoinJoinQueue` {nDenom, m_protxHash, nTime, fReady, vchSig} | CJ server `server.cpp:58`, CJ walletman `walletman.cpp:248` | BRO-violating. Inv type MSG_DSQ (≥70234). |
| ssc | int nItemID, int nCount | SyncManager `sync_manager.cpp:333` (log only) | Sent when serving governance sync. Item IDs: GOVOBJ=10, GOVOBJ_VOTE=11. |
| govsync | uint256 nProp + CBloomFilter | NetGovernance `governance/net_governance.cpp:86` | nProp=0 asks for the full list. Re-asking within the fulfilled-request window costs 20 points. **Sent automatically during sync** with a zero hash and an empty filter (`sync_manager.cpp:32-36`). |
| govobj | `CGovernanceObject` | NetGovernance `:171` | Inv MSG_GOVERNANCE_OBJECT |
| govobjvote | `CGovernanceVote` | NetGovernance `:202` | Inv MSG_GOVERNANCE_OBJECT_VOTE |
| getmnlistd | `CGetSimplifiedMNListDiff` {baseBlockHash, blockHash} | net_processing `:5607` | Errors cost 1 point, except "block data unavailable", which is silent |
| mnlistdiff | `CSimplifiedMNListDiff` | `:5650` | **Receiving one at all costs 100 points** (dashd never asks for it) |
| getqrinfo | `CGetQuorumRotationInfo` {LIMITED_VECTOR baseBlockHashes, blockRequestHash, bool extraShare} | `:5656` | Malformed: 100 points. Legacy construction used if common version < 70236. BRO-violating. |
| qrinfo | `CQuorumRotationInfo` | `:5735` | **Receiving one costs 100 points**. BRO-violating. |
| qsendrecsigs | bool | `:4496` | Sent after a valid MNAUTH to quorum relay members (`evo/mnauth.cpp`). BRO-violating. |
| qfcommit | `llmq::CFinalCommitment` | `CQuorumBlockProcessor::ProcessMessage` `llmq/blockprocessor.cpp:102` | Inv MSG_QUORUM_FINAL_COMMITMENT. Getdata-gated. Re-propagating a stale commitment is banned at ≥70239. |
| qcontrib / qcomplaint / qjustify / qpcommit | `CDKGContribution` / `CDKGComplaint` / `CDKGJustification` / `CDKGPrematureCommitment` | NetDKG `llmq/net_dkg.cpp:58-67`; NetDKGStub when not a masternode | Invs 23–26. BRO-violating. |
| qwatch | empty | NetDKG `:501` | Sent on verack if we watch quorums and the peer is a quorum node. Sets `pnode->qwatch`. BRO-violating. |
| qsigsesann | vector<`CSigSesAnn`> {VARINT sessionId, llmqType, quorumHash, id, msgHash} | NetSigning `llmq/net_signing.cpp:97` | BRO-violating |
| qsigsinv / qgetsigs | vector<`CSigSharesInv`> {VARINT sessionId, compactsize n, auto-bitset} | `:115` | BRO-violating |
| qbsigs | vector<`CBatchedSigShares`> | `:133` | BRO-violating |
| qsigshare | vector<`CSigShare`> {llmqType, quorumHash, quorumMember, id, msgHash, sigShare} | `:66` | **Only when SPORK_21_QUORUM_ALL_CONNECTED is active**. BRO-violating. |
| qsigrec | `CRecoveredSig` {llmqType, quorumHash, id, msgHash, sig} | NetSigning `:49` | Inv MSG_QUORUM_RECOVERED_SIG. Pushed to peers that sent `qsendrecsigs`. BRO-violating. |
| qgetdata / qdata | `CQuorumDataRequest` {llmqType, quorumHash, nDataMask, proTxHash, [nError]} + data | NetQuorum `llmq/net_quorum.cpp:79` / `:204` | BRO-violating |
| clsig | `ChainLockSig` {int32 nHeight, blockHash, BLS sig} | inline `net_processing.cpp:5790` | Ignored if ChainLocks are disabled. **An unrequested (not getdata'd) CLSIG costs 10 points** (`UNREQUESTED_OBJECT_MISBEHAVIOR_SCORE`, `msg_result.h:21`). |
| isdlock | `InstantSendLock` {uint8 nVersion, vector<COutPoint> inputs, txid, cycleHash, sig} | NetInstantSend `instantsend/net_instantsend.cpp:239` | Inv MSG_ISDLOCK. Not announced to block-relay-only peers or peers without `m_relay_txs`, nor to verified masternodes that sent qsendrecsigs (`:1224`). |
| mnauth | `CMNAuth` {uint256 proRegTxHash, BLS sig} | `CMNAuth::ProcessMessage` `evo/mnauth.cpp` | See §4.3 |
| getheaders2 | same as getheaders | `:4756` | Replies with `headers2`, up to 8000 entries (common version ≥70235) or 2000 |
| sendheaders2 | empty | `:4263` | Sent **instead of** `sendheaders` to peers advertising NODE_HEADERS_COMPRESSED |
| headers2 | vector<`CompressibleBlockHeader`> | `:5305` | See §3.4 |
| platformban | `PlatformBanMessage` {protx_hash, int32 requested_height, quorum_hash, BLS sig (non-legacy)} (`masternode/meta.h:178`) | `ProcessPlatformBanMessage` `net_processing.cpp:3837` | Inv MSG_PLATFORM_BAN. Penalties of 1 or 100 points. BRO-violating. |

##### 2.3 Inventory types (`src/protocol.h:495-528`)

| Value | Name | Message for getdata | Notes |
|---|---|---|---|
| 0 | UNDEFINED | | |
| 1 | MSG_TX | tx | |
| 2 | MSG_BLOCK | block | |
| 3 | MSG_FILTERED_BLOCK | merkleblock | getdata only |
| 4, 5 | (legacy txlock request / vote) | | unknown, skipped |
| 6 | MSG_SPORK | spork | allowed during IBD |
| 7–15 | (old budget / masternode) | | unknown |
| 16 | MSG_DSTX | dstx | |
| 17 | MSG_GOVERNANCE_OBJECT | govobj | |
| 18 | MSG_GOVERNANCE_OBJECT_VOTE | govobjvote | |
| 19 | (masternode verify, dead) | | |
| 20 | MSG_CMPCT_BLOCK | cmpctblock | getdata only |
| 21 | MSG_QUORUM_FINAL_COMMITMENT | qfcommit | |
| 22 | (dummy commitment, dead) | | |
| 23 | MSG_QUORUM_CONTRIB | qcontrib | |
| 24 | MSG_QUORUM_COMPLAINT | qcomplaint | |
| 25 | MSG_QUORUM_JUSTIFICATION | qjustify | |
| 26 | MSG_QUORUM_PREMATURE_COMMITMENT | qpcommit | |
| 27 | (debug status, dead) | | |
| 28 | MSG_QUORUM_RECOVERED_SIG | qsigrec | |
| 29 | MSG_CLSIG | clsig | |
| 30 | (non-deterministic ISLOCK, dead) | | |
| 31 | MSG_ISDLOCK | isdlock | |
| 32 | MSG_DSQ | dsq | |
| 33 | MSG_PLATFORM_BAN | platformban | |
| 34 | MSG_ASSET_UNLOCK | **tx** | Version-2 asset unlocks are announced by *instance hash* to peers ≥70242 (`net_processing.cpp:3619-3625`); getdata is answered with `tx` |

- `CInv::IsGenTxMsg()` covers TX, DSTX and ASSET_UNLOCK (`protocol.h:555`).
- Re-request intervals (`GetObjectInterval`, `net_processing.cpp:1594`): QSIGREC 15 s, CLSIG 5 s, ISDLOCK 10 s, everything else 60 s.
- "Getdata-only" types, which must have been requested (`IsGetDataOnlyObject` `:1610`), include CLSIG and the DKG/QFCOMMIT types. QSIGREC, DSQ, ISDLOCK, SPORK and PLATFORMBAN have legitimate unsolicited push paths.

---

#### 3. Transport

##### 3.1 v1 framing (`protocol.h:26-54`, `net.cpp:708-838`)

- Header is 24 bytes: magic(4) ‖ command(12, ASCII 0x20–0x7E, NUL-padded, and everything after the first NUL must be NUL) ‖ LE uint32 length ‖ checksum = first 4 bytes of SHA256d(payload).
- A length over `MAX_PROTOCOL_MESSAGE_LENGTH` = 3 MiB (`net.h:84`) or MAX_SIZE disconnects. So does a bad magic.
- A bad checksum or invalid command **drops the message but keeps the connection** (`reject_message`, `net.cpp:795-806`).
- The receive buffer is allocated at most 256 KiB ahead.

##### 3.2 BIP324 v2 (`src/bip324.cpp`, `net.cpp:885-1690`)

- Enabled by default (`DEFAULT_V2_TRANSPORT=true`). Inbound sockets always use `V2Transport`, which falls back to v1 when the first 16 bytes match magic + `"version\0..."` (`net.cpp:1163`).
- Outbound uses v2 only if both sides advertise NODE_P2P_V2 (`net.cpp:3289`). `-connect` and masternode connections try v2 whenever we support it. On early disconnect, the connection is retried with v1 (`ShouldReconnectV1`, `net.cpp:1647`, reconnection queued at `2160-2171`).
- HKDF salt = `"bitcoin_v2_shared_secret"` ‖ **Dash network magic** (`bip324.cpp:36-38`). Labels: `initiator_L/P`, `responder_L/P`, `garbage_terminators`. REKEY_INTERVAL 224; garbage up to 4095 bytes, random length. The version packet has empty contents.
- **Short message IDs** (`net.cpp:892-1059`):
  - IDs 0–32 follow BIP324 order, with 0 meaning a 12-byte long type follows: 1 addr, 2 block, 3 blocktxn, 4 cmpctblock, **5 "" (feefilter not implemented)**, 6 filteradd, 7 filterclear, 8 filterload, 9 getblocks, 10 getblocktxn, 11 getdata, 12 getheaders, 13 headers, 14 inv, 15 mempool, 16 merkleblock, 17 notfound, 18 ping, 19 pong, 20 sendcmpct, 21 tx, 22 getcfilters, 23 cfilter, 24 getcfheaders, 25 cfheaders, 26 getcfcheckpt, 27 cfcheckpt, 28 addrv2, 29–32 "" (reserved).
  - **Dash IDs 128–168**: 128 spork, 129 getsporks, 130 senddsq, 131 dsa, 132 dsi, 133 dsf, 134 dss, 135 dsc, 136 dssu, 137 dstx, 138 dsq, 139 ssc, 140 govsync, 141 govobj, 142 govobjvote, 143 getmnlistd, 144 mnlistdiff, 145 qsendrecsigs, 146 qfcommit, 147 qcontrib, 148 qcomplaint, 149 qjustify, 150 qpcommit, 151 qwatch, 152 qsigsesann, 153 qsigsinv, 154 qgetsigs, 155 qbsigs, 156 qsigrec, 157 qsigshare, 158 qgetdata, 159 qdata, 160 clsig, 161 isdlock, 162 mnauth, 163 getheaders2, 164 sendheaders2, 165 headers2, 166 getqrinfo, 167 qrinfo, 168 platformban.
  - Receive side: IDs 0–32 and 128–168 are valid. A slot with an empty name produces message type "" and is silently ignored as an unknown command. Any other ID makes the message invalid (dropped).
  - **Send side is gated on the peer's version**: the short form is used only if `m_peer_version ≥ GetMessageMinVersion(type)`. That is 70235 for every ID, *including the Bitcoin ones*, and 70240 for platformban (`net.cpp:989-1006`, `1579`). `m_peer_version` starts at 209 and is updated by `CNode::SetCommonVersion` (`net.h:1042-1048`) when the peer's `version` arrives. Messages sent before that, and all messages to peers below 70235, use the 13-byte long encoding.

##### 3.3 Other transport and feature notes

- **BIP152:** version 1 only. Short txids use the txid (Dash has no segwit). `sendcmpct(false,1)` is sent on verack only if `CanRelay()`.
- **BIP157/158:** serving requires NODE_COMPACT_FILTERS. Limits: getcfilters 1000, getcfheaders 2000, checkpoint interval 1000 (`index/blockfilterindex.h:20`).
- **BIP37:** kept and on by default. `CBloomFilter::IsRelevantAndUpdate` (`common/bloom.cpp:269-305`) also calls `CheckSpecialTransactionMatchesAndUpdate` (`:127-246`). It matches:
  - ProRegTx: collateral outpoint, keyIDOwner, keyIDVoting, owner payout scripts, share refund/reward scripts, share owner keys.
  - ProUpServTx: proTxHash, or the operator payout script (then inserts proTxHash).
  - ProUpRegTx: proTxHash, keyIDVoting, payout scripts.
  - ProUpRevTx, ProDisTx: proTxHash.
  - ProUpShareTx: proTxHash or reward script.
  - ProUpSharedRegTx: proTxHash or keyIDVoting.
  - AssetLock: the credit outputs, treated like normal txouts.
  - Coinbase, quorum commitment, MNHF signal and AssetUnlock: no extra matching.
  - BLOOM_UPDATE_ALL inserts the tx hash or proTxHash as appropriate.
  - The filter is limited to MAX_BLOOM_FILTER_SIZE 36000 bytes and MAX_HASH_FUNCS 50.
- **BIP155:** addrv2 is supported. `sendaddrv2` is sent only to peers with common version ≥70223.
- **Erlay/minisketch:** the code is present (`src/minisketch`, `node/txreconciliation.cpp`) but off by default.
- **No wtxid relay or feefilter.**

##### 3.4 Compressed headers (`headers2`, DIP-25)

Defined in `primitives/block.h:70-190`, logic in `primitives/block.cpp:37-134`.

- Each header is a bit-field byte, then fields that are present only if their flag bit is **set** (a set bit means "present / uncompressed"):
  - bits 0–2: version offset. 0 means an int32 version follows. 1–7 is an index into an MRU list of the last 7 unique versions, which is then moved to the front.
  - bit 3: hashPrevBlock present (otherwise it is the previous header's hash).
  - bit 4: nTime present as uint32. Otherwise an **int16 time offset** from the previous header follows.
  - bit 5: nBits present (otherwise copied from the previous header).
  - merkleRoot and nonce are always present.
- The first header in a message is always fully uncompressed. Deserialization rebuilds the headers in order (`net_processing.cpp:5331-5338`).
- Limits: `MAX_HEADERS_UNCOMPRESSED_RESULT` 2000 and `MAX_HEADERS_COMPRESSED_RESULT` 8000 (`validation.h:81-82`). The 8000 limit applies only for compressed messages with common version ≥70235 (`GetHeadersLimit` `:1209`). Too many costs 20 points.
- getheaders2/headers2 are used instead of getheaders/headers whenever the peer advertises NODE_HEADERS_COMPRESSED (`UsesCompressedHeaders` `:1195`). This applies to initial sync, inv-triggered sync and block announcements (`:6493-6505`).
- There is **no headers presync / HeadersSyncState** (Bitcoin 24+ has one). `MAX_UNCONNECTING_HEADERS` is 10.

---

#### 4. CConnman (`src/net.cpp`)

##### 4.1 Connection types and counts

Connection types are `ConnectionType` {INBOUND, OUTBOUND_FULL_RELAY, MANUAL, FEELER, BLOCK_RELAY, ADDR_FETCH} (`node/connection_types.h:18`). Masternode connections are **OUTBOUND_FULL_RELAY plus flags** on CNode (`net.h:812-819`): `m_masternode_connection`, `m_masternode_probe_connection`, `m_masternode_iqr_connection`. `CanRelay()` = `!m_masternode_connection || m_masternode_iqr_connection` (`net.h:1083`).

| Limit | Value |
|---|---|
| -maxconnections | 125 (`net.h:124`); masternodes must use ≥125 (`init.cpp:1408`). Reduced if file descriptors are short (minimum 150 reserved). |
| Outbound full-relay | 8 |
| Block-relay-only | 2, plus up to 2 anchors |
| Addnode | 8, separate semaphore |
| Feeler | 1, every 2 min (exponential) |
| Extra block-relay peer | every 5 min |
| Extra network peer | every 5 min |
| Desired onion | min(2, maxconn/2) |
| Verified inbound masternode legs per proRegTx | ≤3 (`net.h:118`). These are exempt from the inbound limit and from eviction. |

`ThreadOpenConnections` (`net.cpp:2917-3293`) follows the upstream order: anchors, full-relay, block-relay, stale-tip extra, extra block-relay, feeler, extra-network, onion-only. Dash additions:
- Masternode connections don't count toward the full-relay count (`:3021`).
- It skips addresses of masternodes that already have a verified connection (`:3252-3255`).
- Masternode connections bypass the semaphore (empty grant, `:3670`).

`ThreadOpenMasternodeConnections` (`net.cpp:3397-3584`):
- Runs every 1 s, or every 100 ms right after a connection. Disabled under `-connect`. Waits until the blockchain is synced.
- Priority order: `vPendingMasternodes` (CoinJoin mixing), then quorum members in `masternodeQuorumNodes` (random pick), then pending probes.
- Per-masternode backoff is `LLMQConnectionRetryTimeout` (60 s).
- If a quorum member is connected but hasn't sent MNAUTH within 5 s of its first message, it is dropped.
- Connections go to `netInfo->GetPrimary()`.

Masternode thread active when: `m_masternode_connections = active masternode || -watchquorums || wallet loaded` (`init.cpp:2626-2627`).

Message handler (`ThreadMessageHandler`, `net.cpp:3676`):
- Shuffles nodes each round.
- Calls `SendMessages` on masternode connections at most every 100 ms.
- Waits 100 ms when idle.

##### 4.2 Accept and eviction

`CreateNodeFromAcceptedSocket` (`net.cpp:1875`):
- Applies whitelist permissions. Implicit permissions are Relay (if `-whitelistrelay`, default true), Mempool and NoBan; ForceRelay only if `-whitelistforcerelay`.
- Banned peers are dropped. Discouraged peers are dropped if inbound slots are nearly full; otherwise they are kept with `prefer_evict`.
- Evicts down to `nInbound - nVerifiedInboundMNs < maxInbound`.
- **An active masternode refuses inbound connections until the blockchain is synced.**
- Sets TCP_NODELAY.

`GetEvictionCandidates` (`:1775`): on a masternode, peers are protected for `INBOUND_EVICTION_PROTECTION_TIME` = 1 s after connecting, unless their first post-verack message was not MNAUTH. MNAUTH-verified peers are always protected.

`SelectNodeToEvict` (`node/eviction.cpp`) is upstream:
- protect noban and outbound
- protect 4 by netgroup, 8 by ping, 4 by tx time, 8 block-relay by time, 4 by block time
- protect by network ratio (CJDNS, I2P, localhost, onion)
- then pick from the largest netgroup

Disconnect behavior (Dash-specific): when we initiate a disconnect, the socket lingers about 100 ms and `shutdown(SD_SEND)` is called after the send queue flushes (`net.cpp:2115-2136`).

`InactivityCheck` (`:2298`): `-peertimeout` 60 s for the first send/recv; 20 min of send or receive inactivity; handshake timeout. The ping timeout is 20 min.

`CMasternodeUtils::DoMaintenance` (`masternode/utils.cpp`, every 1 min):
- Only runs once enough non-masternode outbound peers exist.
- Disconnects probes after 5 s.
- Disconnects unverified masternode connections after 5 s, unless they are `qwatch` peers.
- Disconnects verified outbound masternode connections that are neither quorum nodes nor relay members, except mixing masternodes.

##### 4.3 MNAUTH handshake (`evo/mnauth.cpp`)

1. Each side puts a random 32-byte challenge in `version`.
2. After verack, a masternode (unless the connection is a probe) sends `mnauth` {proRegTxHash, sig}.
   - sig = BLS (basic scheme) over SerializeHash(tuple(operatorPubKey, *peer's* challenge, IsInbound, our version)). `-pushversion` is allowed off mainnet.
   - The verifier uses (registered operator key, its own sent challenge, !IsInbound, peer.nVersion).
3. Penalties:
   - Duplicate MNAUTH, a peer missing NETWORK or BLOOM services, a null proTx hash, or an invalid signature: 100 points.
   - Unknown masternode or failed signature verification: 10 points.
   - MNAUTH is ignored entirely until the blockchain is synced.
4. On a successful probe, the connection is disconnected.
5. Duplicate legs are resolved with `DeterministicOutboundConnection(a,b)` = the proTx whose `SerializeHash(min, max, self)` is smaller (`llmq/utils.cpp:732`).
6. If the peer is a quorum relay member, we send `qsendrecsigs(true)` and set the IQR flag.
7. A probe connection whose first message isn't MNAUTH is disconnected (`net_processing.cpp:4370`).

Quorum connection sets (`llmq/utils.cpp:754-829`):
- With "all members connected" enabled (spork 21 / LLMQ params): connect to all members, and only the deterministic outbound side dials.
- Otherwise: ring relay to indexes (i+2^k) mod n.
- Watch connections (`CalcDeterministicWatchConnections`) use a random seed.

##### 4.4 Discovery, seeds and addnode

- **DNS seeds:** queried immediately if addrman is empty or `-forcednsseed`. Otherwise after 11 s (<1000 addresses) or 5 min, and only if fewer than 2 full-outbound peers. 3 seeds at a time, max 32 IPs per seed, random age of 3–7 days. If there is a proxy or no results, an ADDR_FETCH connection to the seed is used instead (`net.cpp:2680-2793`).
- **Fixed seeds:** added after 60 s if a reachable network is still empty, or immediately if there is no dnsseed, seednode or addnode.
- `-connect` disables automatic, DNS and masternode connections.
- `-seednode` creates ADDR_FETCH connections, disconnected after an addr with >1 entry or 10×30 s.
- `-addnode` connections are MANUAL and never punished.

##### 4.5 Permissions, banning and misbehavior

- Permission flags (`net_permissions.h`): bloomfilter(1<<1), forcerelay(1<<2|relay), relay(1<<3), noban(1<<4|download), mempool(1<<5), download(1<<6), addr(1<<7), all. Implicit is 1<<31.
- Ban storage: `banlist.json` with key `"banned_nets"`, entries {version, ban_created, banned_until, address} (`net_types.cpp`, `addrdb.cpp:135-180`). `banlist.dat` is ignored. Default ban time is 24 h; bans are dumped every 15 min.
- Discouragement is a rolling bloom filter of 50000 entries at 1e-6 (`banman.h:94`).
- **Misbehavior is score-based** (`net_processing.cpp:1932-1954`). Points accumulate and reaching `DISCOURAGEMENT_THRESHOLD` = 100 (`net_processing.h:47`) sets `m_should_discourage`. `MaybeDiscourageAndDisconnect` then exempts noban and manual peers, disconnects local peers without discouraging them, and otherwise discourages and disconnects.
- Score table: oversized inv/getdata/addr/headers/notfound = 20; unconnecting headers = 20; invalid header = 10; non-continuous headers = 20; bad filter = 100; unrequested mnlistdiff/qrinfo = 100; invalid spork = 100; getmnlistd/getqrinfo errors = 1; unrequested clsig = 10; mnauth = 100 or 10; govsync repeat = 20; devnet name mismatch = 100 outbound / 1 inbound; block/tx invalidity uses upstream values (100/10).

##### 4.6 netfulfilledman (`netfulfilledman.cpp`)

- Keyed by CService, with string request names: `"spork-sync"`, `"full-sync"`, `"mempool-sync"`, `"governance-sync"`, `"govsync"`, `"govsync-votes-<hash>"`.
- Persisted to `netfulfilled.dat`; maintenance runs every 1 min.
- A peer already marked `full-sync` is disconnected by the sync tick (`sync_manager.cpp:196-201`).

##### 4.7 Other networking

- **Socket loop:** `-socketevents` = epoll (Linux, edge-triggered events and a wakeup pipe), kqueue (BSD/macOS), select/poll. The select timeout is 50 ms, or 500 ms with the wakeup pipe (`net.cpp:114-119`).
- **Buffers:** `-maxreceivebuffer` 5000 KB, `-maxsendbuffer` 1000 KB.
- **Upload limit:** `-maxuploadtarget` (default 0 = unlimited, over a 24 h window). **Masternodes must leave it at 0** (`init.cpp:1423`).
- **Tor:** control port 9051; `ADD_ONION NEW:ED25519-V3 Port=<defaultport>,127.0.0.1:<9996…>`; `-listenonion` defaults to true.
- **I2P:** SAM 3.1 (`HELLO VERSION MIN=3.1 MAX=3.1`), `DEST GENERATE SIGNATURE_TYPE=7`, port 0, accepts incoming by default.
- **Port mapping:** UPnP and NAT-PMP both default to off.
- **CJDNS:** supported (`MaybeFlipIPv6toCJDNS`).
- **asmap:** health check every 24 h.

---

#### 5. addrman and on-disk files

- **Buckets:** new 1024 × 64, tried 256 × 64. Tried buckets per group 8; new buckets per source group 64; buckets per address 8. Horizon 30 days, 3 retries, 10 max failures, min fail 7 days, replacement 4 h, test window 40 min, up to 10 tried collisions (`addrman.cpp:30-48`, `addrman_impl.h:27-34`). The entry key is the full CService (IP+port).
- **peers.dat** (`addrman.cpp:177-233`, `addrdb.cpp:37-50`):
  - File = magic(4) ‖ data ‖ SHA256d of everything before it.
  - data = u8 format (=4, V4_MULTIPORT) ‖ u8 (32 + lowest_compatible = 36) ‖ uint256 nKey ‖ int nNew ‖ int nTried ‖ int (1024 ^ 1<<30) ‖ nNew × AddrInfo ‖ nTried × AddrInfo ‖ 1024 × {int size, size × int index} ‖ uint256 asmap checksum.
  - AddrInfo = CAddress in disk format (u32 version where bit 29 means addrv2, then the addrv2 encoding) ‖ CNetAddr source ‖ int64 m_last_success ‖ int nAttempts.
  - Dumped every 15 min.
- **anchors.dat:** the same magic+hash wrapper around vector<CAddress> (ADDRV2). Up to 2 block-relay peers are written at shutdown; the file is read and deleted at startup (`addrdb.cpp:233-251`).
- **Other files Dash writes:** `banlist.json`, `netfulfilled.dat`, `mncache.dat`, `governance.dat`.

---

#### 6. net_processing flow

##### 6.1 Handshake message order

**Outbound (we dial):**
1. `version` is sent from `InitializeNode`.
2. On receiving the peer's `version`: `SetCommonVersion`, then `sendaddrv2` (≥70223), then [`sendtxrcncl` if enabled], then `verack`, then `getaddr` (outbound non-block-relay; adds +1000 to the addr token bucket).
3. Feelers disconnect here.
4. On receiving `verack`:
   - [`mnauth` if we are a masternode and this isn't a probe]
   - `sendheaders2` (peer has bit 11) **or** `sendheaders`
   - `sendcmpct(0,1)` if `CanRelay()`
   - `senddsq(true)` unless the connection rejects transactions
   - [`qwatch`]
5. `SendMessages` then sends:
   - an immediate `ping`
   - `getheaders2`/`getheaders`, if this is the sync peer or we are near the tip
   - inv trickles

**Inbound:** receive `version`, then send `version`, `sendaddrv2`, `verack`. After receiving `verack`, the same post-verack set as above.

Messages other than version/verack/sendheaders(2)/sendcmpct/sendaddrv2/sendtxrcncl are ignored before verack. Anything before `version` is ignored.

**Background sync** (`SyncManager`, tick every 6 s, `sync_manager.cpp:150-320`). For each fully connected, relay-capable peer (an active masternode skips inbound peers):
1. Send `getsporks` once per address.
2. Once the chain tip is reached (6 s with >3 peers, otherwise 30 s): send `mempool` to every outbound non-block-relay peer (`-syncmempool`, default true).
3. Governance asset: `govsync(0, empty bloom)` to one peer per tick, then per-object vote requests (up to 3 peers per hash on real networks).
4. With no activity for 1 h, sync restarts (not on masternodes).

##### 6.2 Relay

- **Object requests** are a generalised TxRequestTracker (`m_object_request`) covering every inv type:
  - `MAX_PEER_OBJECT_ANNOUNCEMENTS` = max(5000, 2×MAX_INV_SZ) = **100000**. Dash does not reduce it, so governance vote sync works.
  - 100 in flight before the 2 s overload delay.
  - `NONPREF_PEER_TX_DELAY` 2 s, applied only to MSG_TX, only from non-preferred peers, and **never when we are a masternode** (`:1634-1663`).
- **Trickle intervals:** inbound 5 s (shared Poisson timer), outbound 2 s Poisson, **1 s for outbound verified masternodes**. **No trickle at all for masternode-to-masternode or noban peers** (`:6590-6604`).
- **Rates:** 7 tx/s; at most `4·7·5·MaxBlockSize/1MB + size/1000·5`, capped at 1000 per trickle. mapRelay is kept 15 min; unconditional relay after 2 min.
- **Non-tx inventory** (`vInventoryOtherToSend`: sporks, governance, CLSIG, ISDLOCK, QFCOMMIT, DSQ…) **is also sent to block-relay-only peers**. Exceptions: types in the blocks-only list, and ISDLOCK, which is skipped unless `m_relay_txs` is set and the peer isn't block-relay-only (`:6728-6745`). These items go out on every SendMessages call, not on the trickle timer.
- **DSQ:** to INV-mode peers via inv; to ALL-mode peers as a direct `dsq` push (`:2551-2576`).
- **Recovered sigs:** pushed proactively to peers with `m_wants_recsigs`.
- **ISDLOCK:** announced through a filter (the peer's bloom filter must match the tx). Not announced to verified masternodes that opted in to recsigs.
- **Orphans:** `-maxorphantxsize` 10 MB, expire after 20 min, swept every 5 min.

##### 6.3 Blocks and timeouts (`net_processing.cpp:85-222`)

| Constant | Value |
|---|---|
| PING_INTERVAL | 2 min |
| TIMEOUT_INTERVAL (ping / inactivity) | 20 min |
| -peertimeout | 60 s |
| MAX_INV_SZ | 50000 |
| MAX_GETDATA_SZ (outgoing batches) | 1000 |
| MAX_LOCATOR_SZ | 101 |
| MAX_ADDR_TO_SEND | 1000 |
| MAX_PCT_ADDR_TO_SEND | 23% |
| Addr relay rate | 0.1/s |
| AVG_ADDRESS_BROADCAST_INTERVAL | 30 s |
| Local address broadcast | 24 h |
| MAX_BLOCKS_IN_TRANSIT_PER_PEER | 16 |
| BLOCK_DOWNLOAD_WINDOW | 1024 |
| Stalling timeout | 2 s, doubling to a max of 64 s |
| Block download timeout | 150 s × (1 + 0.5·N other peers) |
| HEADERS_DOWNLOAD_TIMEOUT | 15 min + 1 ms per expected header |
| HEADERS_RESPONSE_TIME | 2 min |
| CHAIN_SYNC_TIMEOUT | 20 min |
| STALE_CHECK_INTERVAL | 150 s |
| EXTRA_PEER_CHECK_INTERVAL | 45 s |
| MINIMUM_CONNECT_TIME | 30 s |
| MAX_OUTBOUND_PEERS_TO_PROTECT_FROM_DISCONNECT | 4 |
| MAX_BLOCKS_TO_ANNOUNCE | 8 |
| NODE_NETWORK_LIMITED_MIN_BLOCKS | 288 |
| HISTORICAL_BLOCK_AGE | 7 days |
| Unconnecting headers before penalty | 10 |

Extra-outbound eviction never picks masternode connections (`:6069`). Up to `GetMaxOutboundOnionNodeCount` onion peers are also protected.

##### 6.4 Dash subsystem hooks

`PeerManager` implements `PeerManagerInternal` (`net_processing.h:83-121`: PeerMisbehaving, PeerRelayInv, PeerRelayInvFiltered, PeerRelayDSQ, PeerRelayRecoveredSig, PeerConsumeGetDataResponse, PeerPostProcessMessage…).

`NetHandler` (`:124-145`) is the extension interface: `ProcessMessage`, `AlreadyHave`, `ProcessGetData`, `Schedule`, `Start`/`Stop`.

Handlers registered in `init.cpp:2208-2281`:
- NetInstantSend
- llmq::NetSigning
- llmq::NetQuorum
- llmq::NetDKG (masternode or watcher) or NetDKGStub
- CCoinJoinServer (masternode only)
- NetGovernance (always registered)
- SyncManager

`ProcessGetData` (`:2931-3093`) serves directly: TX/DSTX/ASSET_UNLOCK, SPORK, QFCOMMIT, QSIGREC, CLSIG, ISDLOCK, DSQ (wallet manager), PLATFORMBAN. Other types go to the handlers; anything unanswered goes into `notfound`.

`MessageProcessingResult` (`msg_result.h`) carries misbehavior, inventory to relay, and requests to erase.

---

#### Notes for a Rust reimplementation (fingerprinting risks)

1. **`version` layout must be exact.**
   - Append a uint256 challenge (random, never zero in practice) and the `fMasternode` byte after `relay`.
   - addrMe carries *our* services plus an all-zero IPv6 and port 0.
   - addrYou is zeroed if the peer address isn't routable, is a proxy, or isn't addrv1-compatible.
   - Subver is `/Dash Core:24.1.0/`, plus `(devnet.<name>; uacomments)` when applicable.
   - Write it with version 209 framing; start_height is our best height.
2. **Service bits:** a default node advertises 0x1C05 (NETWORK, BLOOM, NETWORK_LIMITED, HEADERS_COMPRESSED, P2P_V2). A masternode adds COMPACT_FILTERS (0x1C45). Leaving out BLOOM or HEADERS_COMPRESSED is immediately visible. NETWORK_LIMITED is set *together with* NETWORK.
3. **Post-verack sequence and ordering:**
   - `sendaddrv2` → `verack` → `getaddr` (outbound), then `sendheaders2` **instead of** `sendheaders` for bit-11 peers, then `sendcmpct(0,1)`, then `senddsq(1)`.
   - No `feefilter`, no `wtxidrelay`, no `sendtxrcncl` by default.
   - Never send `sendcmpct` on a non-IQR masternode connection.
4. **Automatic requests dashd makes:** `getsporks` to every relay peer within about 6 s; `mempool` to outbound peers after headers/blocks sync; `govsync` with a zero hash and an empty `CBloomFilter` (serialized as empty vData plus zero fields); then per-object vote syncs. If these are missing, or come at the wrong cadence, a peer can tell. Also, after a `full-sync` with a peer, dashd disconnects it.
5. **BIP324:**
   - The salt uses the Dash magic.
   - Use short IDs *only* when common version ≥70235 (≥70240 for platformban), for Bitcoin IDs too. Before `version` is received, everything is long-encoded.
   - When receiving, accept IDs 0–32 and 128–168. Treat 5 and 29–32 as an empty type and ignore it silently; reject other IDs.
   - Garbage length is uniform in 0..4095.
   - Outbound v2 is used only if the address advertises bit 12. Fall back to v1 on early disconnect.
6. **Penalties.**
   - Use score points with a threshold of 100, not upstream's instant discouragement.
   - An unsolicited `mnlistdiff` or `qrinfo` costs 100. An unrequested `clsig` costs 10.
   - A malformed v1 message (bad checksum or command) is dropped without disconnecting.
   - Unknown commands are silently ignored.
   - A non-whitelisted `mempool` request without NODE_BLOOM, or `filter*` without NODE_BLOOM, means disconnect.
   - Messages that violate blocks-only on a block-relay-only connection mean disconnect. Note that **`tx` is in that list but `clsig`, `spork`, `govobj` and `isdlock` are not**.
7. **Relay timing:** Poisson trickles of 5 s inbound (shared timer), 2 s outbound, 1 s to verified outbound masternodes, and no delay masternode-to-masternode. Non-tx Dash invs go out on every SendMessages call (100 ms cadence, unaffected by the trickle) and also reach block-relay-only peers. `mempool` replies include matching ISDLOCK invs and the best CLSIG inv. `merkleblock` replies are followed by `tx` messages and then `isdlock` messages.
8. **Headers:** reproduce the headers2 encoding bit for bit, including the inverted "bit set = field present" convention, the 7-entry MRU version list, and int16 time deltas. Limits are 8000 compressed / 2000 uncompressed. There is no presync. `getheaders` below minimum chain work returns an empty `headers`/`headers2`.
9. **Masternode behavior:**
   - Inbound connections are refused until synced.
   - Inbound peers are protected for 1 s while MNAUTH arrives.
   - Verified inbound masternode legs (≤3 per proTx) are exempt from limits.
   - Masternode connections bypass the outbound semaphore.
   - 100 ms SendMessages cadence on masternode connections.
   - The MNAUTH sign-hash tuple must match exactly (pubkey, challenge, inbound flag, version).
   - Probes disconnect after MNAUTH.
10. **Disconnect mechanics:** about 100 ms linger plus `shutdown(SHUT_WR)` before close. The addr cache lives 21–27 h. Feelers disconnect right after receiving `version`. ADDR_FETCH connections disconnect after an addr message with more than 1 entry.
11. **Persisted formats:** peers.dat is V4_MULTIPORT with a compatibility byte of 36. Every DB file is magic ‖ data ‖ SHA256d. Bans go in `banlist.json` under `banned_nets`. anchors.dat is deleted after it is read.

### A.3 Evo (special txs, DML, SML, CbTx, credit pool, MNHF, netinfo, shared MNs), masternode payments/sync/meta, active masternode, sporks

*Section numbers below (§1, §2, …) are local to A.3.*

This is a read-only review. All paths are under `/work/home/agent/workspace/dash/src/` unless stated otherwise.

**Before you start, note this.** develop has merged a "shared masternode" feature (commit `2860d230d8`, "consensus: add shared collateral template script and new special tx types"). It adds three special transaction types (10, 11, 12), a share table on ProRegTx v3, a collateral covenant script, and DIP‑0026 multi-payouts. All of it is gated on `DEPLOYMENT_V24`. The rest of this report covers these in full, because the sibling rust-dashcore repo has none of it (see §15).

---

#### 0. File inventory (wc -l)

| Path | Lines | Role |
|---|---|---|
| `primitives/transaction.h` / `.cpp` | 436 / — | `TxType` enum, special-tx serialization (`n32bitVersion = nVersion \| nType<<16`), the asset-unlock "stable txid" hashing |
| `evo/specialtx.h` / `.cpp` | 48 / 26 | `GetTxPayload<T>` (must consume the whole payload, `ds.empty()`), `CalcTxInputsHash`, `CalcTxOutputsHash` |
| `evo/types.h` | 39 | `ProTxVersion {LegacyBLS=1, BasicBLS=2, ExtAddr=3}`, `GetMax()` |
| `evo/dmn_types.h` | 67 | `MnType {Regular=0, Evo=1}`, collateral and voting weights |
| `evo/providertx.h` / `.cpp` / `_util.cpp` | 568 / 596 / 115 | ProTx payload classes, `IsTriviallyValid`, payout/share-list rules, consent and dissolve digests, `SplitAmountByShares` |
| `evo/sharedcollateral.h` | 29 | the shared-collateral template script |
| `evo/specialtxman.h` / `.cpp` | 176 / 1868 | `CheckSpecialTx`, all `CheckPro*Tx`, `BuildNewListFromBlock`, `ProcessSpecialTxsInBlock`, the shared-collateral covenants, the CbTx chainlock check |
| `evo/dmnstate.h` / `.cpp` | 485 / 172 | `CDeterministicMNState`, `CDeterministicMNStateDiff` (and legacy diff) |
| `evo/deterministicmns.h` / `.cpp` | 859 / 1527 | `CDeterministicMN`, `CDeterministicMNList`, `CDeterministicMNManager`, payee selection, PoSe, evodb snapshot/diff, migrations and repair |
| `evo/simplifiedmns.h` / `.cpp` | 127 / 146 | SML entry/list, `merkleRootMNList` |
| `evo/smldiff.h` / `.cpp` | 103 / 252 | `getmnlistd` / `mnlistdiff` (`CSimplifiedMNListDiff`) |
| `evo/cbtx.h` / `.cpp` | 89 / 211 | `CCbTx` v1–v4, `CheckCbTx`, `merkleRootQuorums`, `merkleRootAssetUnlocks` |
| `evo/creditpool.h` / `.cpp` | 194 / 405 | credit pool and withdrawal limits |
| `evo/assetlocktx.h` / `.cpp` | 178 / 273 | AssetLock / AssetUnlock payloads and checks |
| `evo/mnhftx.h` / `.cpp` | 164 / 420 | MNHF (EHF) signal tx and `CMNHFManager` |
| `evo/netinfo.h` / `.cpp` | 508 / 678 | `MnNetInfo` (legacy) and `ExtNetInfo` (v3) address containers and validation |
| `evo/evodb.h` / `.cpp` | 287 / 244 | `CEvoDB` (LevelDB at `<datadir>/evodb`) |
| `evo/mnauth.h` / `.cpp` | 70 / 210 | MNAUTH p2p message |
| `evo/snapshot.h` / `.cpp` | 745 / 387 | assumeutxo "EvoSnapshot" codec (marker `"DASHEVO\0"`, version 3); not consensus |
| `evo/specialtx_filter.cpp` | 171 | special-tx elements added to BIP158 filters and bloom filters |
| `evo/chainhelper.cpp` / `.h` | 104 / 92 | `CChainstateHelper` (wires payments, special tx, superblocks, EHF; `IsSuperblockValidationRequired`) |
| `evo/core_write.cpp` | 436 | JSON only |
| `evo/providertx_service.cpp` | 1351 | wallet/RPC builders for ProTx, including shared_*; not consensus |
| `masternode/payments.cpp` / `.h` | 474 / 105 | block reward split, MN payee outputs, block value/payee validation |
| `masternode/meta.cpp` / `.h` | 267 / 253 | `mncache.dat`, `PlatformBanMessage` |
| `masternode/sync.cpp` / `.h` | 158 / 100 | `CMasternodeSync` state machine |
| `node/sync_manager.cpp` | 358 | the sync tick (asset switching, getsporks, governance sync) |
| `masternode/utils.cpp` | 83 | disconnects stale MN connections |
| `active/masternode.cpp` / `.h` | 310 / 77 | `CActiveMasternodeManager` |
| `active/context.cpp` / `.h` | 135 / 110 | `ActiveContext` (MN-only components) |
| `active/dkgsession*.cpp` | 750 / 273 | the MN's DKG participation (LLMQ scope) |
| `llmq/ehf_signals.cpp` | 140 | MN-side signing of MNHF signals |
| `spork.cpp` / `.h` | 337 / 309 | sporks |
| `netfulfilledman.cpp` / `.h` | 97 / 71 | `netfulfilled.dat` |

---

#### 1. Special transaction framing and type table

- **Serialization** (`primitives/transaction.h:271,357-362`): the first int32 is `nVersion | (nType<<16)`. On read, `nVersion` is the low 16 bits and `nType` the high 16 bits.
- `vExtraPayload` (a CompactSize-prefixed byte vector) is appended after `nLockTime` only if `nVersion >= 3 && nType != 0`.
- `MAX_TX_EXTRA_PAYLOAD = 10000` (`consensus/consensus.h:22`, checked in `CheckTransaction`).
- Empty `vin`/`vout` is allowed for types 6 and 7. Empty `vin` is allowed for type 9 (`consensus/tx_check.cpp`).
- **Coinbase** (`tx_check.cpp`): a type‑5 coinbase may have a scriptSig of 1–100 bytes; otherwise 2–100.
- **ContextualCheckTransaction** (`validation.cpp:351-380`): after DIP3 (`ActiveAfter(pindexPrev)`), a v3 tx must have nType in {0..12} or it fails with "bad-txns-type". A coinbase with v3 must be type 5. A non-v3 tx with nType≠0 also fails "bad-txns-type".
- **ContextualCheckBlock** (`validation.cpp:4587`): after DIP3 the coinbase must be type 5 ("bad-cb-type"), and the BIP34 height-in-coinbase rule is disabled.
- **CheckSpecialTxInner** (`evo/specialtxman.cpp:226`):
  - It returns OK if `!HasExtraPayloadField()`.
  - Otherwise DIP3 must be `ActiveAfter(pindexPrev)` ("bad-tx-type-dip3-inactive").
  - It then dispatches on type. Any exception becomes "failed-check-special-tx".
- **GetTxPayload** fails unless the payload is consumed exactly (no trailing bytes).
- **Payload version gate for ProTx types** (`GetValidatedPayload`, `specialtxman.cpp:1103`): `payload.nVersion <= ProTxVersion::GetMax(v19ActiveAfter(prev), is_v24_active)`.
  - pre-V19: max is 1.
  - V19: max is 2.
  - V19 and V24: max is 3.
- **`is_v24_active`** is always `DeploymentActiveAfter(pindex->pprev, V24)` (`validation.cpp:2660`).

| nType | Name | Payload class | Payload versions |
|---|---|---|---|
| 0 | NORMAL | — | — |
| 1 | PROVIDER_REGISTER | `CProRegTx` | 1 LegacyBLS, 2 BasicBLS (V19), 3 ExtAddr (V24) |
| 2 | PROVIDER_UPDATE_SERVICE | `CProUpServTx` | 1/2/3 as above |
| 3 | PROVIDER_UPDATE_REGISTRAR | `CProUpRegTx` | 1/2/3 |
| 4 | PROVIDER_UPDATE_REVOKE | `CProUpRevTx` | 1/2/3 |
| 5 | COINBASE | `CCbTx` | 1..4 |
| 6 | QUORUM_COMMITMENT | `llmq::CFinalCommitmentTxPayload` {u16 nVersion=1, u32 nHeight, CFinalCommitment}; commitment versions 1–4 (legacy/basic × indexed/non-indexed) | — |
| 7 | MNHF_SIGNAL | `MNHFTxPayload` | 1 |
| 8 | ASSET_LOCK | `CAssetLockPayload` | 1, 2 (V24) |
| 9 | ASSET_UNLOCK | `CAssetUnlockPayload` | 1, 2 (V24; stable txid) |
| 10 | PROVIDER_DISSOLVE | `CProDisTx` | 1 (V24) |
| 11 | PROVIDER_UPDATE_SHARE | `CProUpShareTx` | 1 (V24) |
| 12 | PROVIDER_UPDATE_SHARED_REGISTRAR | `CProUpSharedRegTx` | 1 (V24) |

`CalcTxInputsHash` is `SHA256d` over the serialized `prevout`s (`CHashWriter SER_GETHASH`). `CalcTxOutputsHash` is `SHA256d` over the serialized `CTxOut`s. Every ProTx except ProDisTx carries `inputsHash` (replay protection, "bad-protx-inputs-hash").

##### 1.1 Deployment gates and mainnet heights

From `chainparams.cpp:168-196`:

| Deployment | Mainnet height | What it gates in this scope |
|---|---|---|
| DIP0003 | 1028160 | special txs, CbTx v1, DMN list |
| DIP0003 enforcement | 1047200 (hash `000000000000002d1734…f81`) | MN payee enforcement. Before it: `keyIDOwner == keyIDVoting` is required ("bad-protx-key-not-same"). The enforcement block hash is checked in `CDeterministicMNManager::ProcessBlock`. |
| DIP0008 | 1088640 | CbTx ≥ v2 (merkleRootQuorums). The check uses `DeploymentActiveAt(*pindexPrev)` (note the off-by-one versus `After`). |
| BRR | 1374912 | MN reward reallocation schedule |
| V19 | 1899072 | BasicBLS ProTx v2, EvoNodes, global `bls_legacy_scheme=false` from the block after activation |
| V20 | 1987776 | CbTx v3 (best CL and credit pool balance), credit pool, treasury 20%, fixed subsidy base, MNHF signals counted |
| MN_RR | 2128896 | platform share (37.5% of MN subsidy reward) goes to the credit pool; MN reward = 75% of block reward; evonode 4-in-a-row payments end |
| WITHDRAWALS | 2201472 | unlock limit 2000 DASH |
| V24 (BIP9 + EHF, bit 13) | mainnet NEVER_ACTIVE (TODO); testnet start 2026‑09‑24; window 4032 / threshold 3226→2420 / falloff 5 | ExtAddr v3, multi-payouts, shared MNs, CbTx v4, asset lock/unlock v2, unlock limit v24 rule, legacy BLS registration disallowed, strict multiplicity in payee checks |

Asset lock/unlock and MNHF have **no explicit V20 gate** in `CheckSpecialTx` any more (it was buried). The historical chain has none of these txs before V20. A strict reimplementation should gate type 8/9 on V20 itself, or simply mirror Core.

---

#### 2. ProTx payloads (exact serialization)

`READWRITE` uses Bitcoin serialization. `CScript` is a CompactSize-prefixed vector. `CKeyID` is 20 bytes. A BLS public key is 48 bytes and a BLS signature 96 bytes; their encoding (legacy vs basic) is chosen by the version wrapper (`legacy = nVersion==1`).

**ProTx version semantics:**
- v1: legacy BLS.
- v2: basic BLS, and adds `nType` (Evo) fields.
- v3: ExtNetInfo replaces MnNetInfo, the payouts list replaces scriptPayout, the share table is added, and the Platform ports move into netInfo.

On read, an unknown version (0 or >3) stops deserialization early. The payload is then partial, so `GetTxPayload` fails: if bytes remain, `ds.empty()` is false.

##### 2.1 CProRegTx (type 1) — `evo/providertx.h:137-245`
```
u16 nVersion
u16 nType (MnType)   u16 nMode(=0)   COutPoint collateralOutpoint
netInfo: v<3 → MnNetInfo (CService, ADDRv1 18 bytes); v3 → ExtNetInfo
CKeyID keyIDOwner   BLSPubKey pubKeyOperator(legacy iff v1)   CKeyID keyIDVoting   u16 nOperatorReward
if v>=3:  u8 payouts_count; payouts[]{CScript scriptPayout, u16 reward}
          u8 shares_count;  shares[]{i64 amount, CScript scriptRefund, CScript scriptReward, CKeyID keyIDOwner}
          [65-byte compact sig] × shares_count (vchJoinSigs, raw, no length prefix)
          u32 nEarlyPeriodBlocks, i64 nEarlyPenalty
else:     CScript scriptPayout
uint256 inputsHash
if nType==Evo: uint160 platformNodeID; if v<3: u16 platformP2PPort, u16 platformHTTPPort
if !SER_GETHASH: vector<u8> vchSig
```
The payouts and shares counts are **u8, not CompactSize**.

**IsTriviallyValid** (`providertx.cpp:265`):
- version must be 1..3.
- Evo requires v≥2 ("bad-protx-evo-version").
- `nType < 2`; `nMode == 0`.
- **Shared** (`shares` non-empty): `nType` must be Regular; the collateral must be internal (`hash` null); `keyIDOwner` must be null; `payouts` must be empty.
- **Non-shared**: `nEarlyPeriodBlocks == nEarlyPenalty == 0`; `keyIDOwner` non-null.
- The operator key must be valid and `keyIDVoting` non-null.
- `pubKeyOperator.IsLegacy() == (v==1)`.
- The share list or payout list must be trivially valid (§2.6, §2.7).
- `netInfo->CanStorePlatform() == (v>=3)` ("bad-protx-netinfo-version").
- netInfo, if non-empty, must be trivially valid.
- Each entry must satisfy `IsTriviallyValid()`.
- `nOperatorReward <= 10000`.

**CheckProRegTx** (`specialtxman.cpp:1151`):
1. If V24 is active, v<2 is rejected ("bad-protx-version-disallowed"). No new legacy-BLS registrations after V24.
2. `CheckProviderNetworkFields(..., allow_empty=true)` (§2.8). An empty netInfo is allowed; the MN then starts PoSe-banned.
3. Expected collateral is exactly `1000*COIN` (Regular) or `4000*COIN` (Evo).
   - **External** (`collateralOutpoint.hash != 0`):
     - The coin must exist unspent with exactly that value.
     - It must have a destination ("bad-protx-collateral-dest"), and that destination must be P2PKH ("bad-protx-collateral-pkh").
     - Proof is `CMessageSigner::VerifyMessage(keyID, vchSig, MakeSignString())`, i.e. `SHA256d(ser("DarkCoin Signed Message:\n") || ser(str))` with compact-sig recovery.
     - The sign string is `payoutStr|nOperatorReward|ownerAddr|votingAddr|SerializeHash(payload)`. For v3, payoutStr is the `PayoutListToString` "addr:reward,…"; otherwise it is the encoded address (or hex script).
   - **Internal** (`hash` null): `n < vout.size()` and `vout[n].nValue == expected`.
     - Shared: the script must be exactly the template script (§3.1).
     - Otherwise a destination must be extractable (P2SH allowed).
     - `vchSig` must be empty.
     - Shared: each `vchJoinSigs[i]` must pass `VerifyHashCanonical(MakeSharedRegConsentHash(tx), shares[i].keyIDOwner)` ("bad-protx-shares-sig").
4. Non-shared: `IsPayoutListKeySafe`. The collateral dest must not equal P2PKH(owner) or P2PKH(voting) ("bad-protx-collateral-reuse"). For v3 only, no payout may pay the collateral dest.
5. Against `mnList(pindexPrev)`:
   - Every netInfo entry (CService or DomainPort) must not be a unique property of an MN with a *different collateral*. The same collateral is allowed (re-registration replaces the MN).
   - Owner key, every share owner key, and operator key must not be unique properties ("bad-protx-dup-key"). This holds even when replacing an MN.
   - If V24: `HasOperatorKeyUnderAnyScheme(pubkey, self=0)` must be false. It probes both legacy and basic encodings.
   - Evo: `platformNodeID` must not be in use ("bad-protx-dup-platformnodeid").
   - Before DIP3 enforcement: owner == voting.
6. `inputsHash` must equal `CalcTxInputsHash(tx)`.

`check_sigs` is false when validating blocks below assumevalid (`fScriptChecks`).

##### 2.2 CProUpServTx (type 2)
```
u16 nVersion; if v>=2: u16 nType; uint256 proTxHash; netInfo (Ext iff v3); CScript scriptOperatorPayout; uint256 inputsHash;
if nType==Evo: uint160 platformNodeID; if v<3: u16 p2pPort, u16 httpPort
if !GETHASH: BLSSig sig (legacy iff v1)
```
**Rules** (`specialtxman.cpp:1311`):
- Trivial: version and type checks; netInfo non-empty; CORE_P2P present; Regular has no platform entries; ExtAddr Evo must have both PLATFORM_P2P and PLATFORM_HTTPS.
- `CheckProviderNetworkFields(allow_empty=false)`.
- The MN must exist, and `nType` must equal `dmn->nType` ("bad-protx-type-mismatch").
- `IsVersionChangeValid`: with V24, a state version ≥2 cannot take a v1 tx ("bad-protx-version-downgrade").
- With V24, an Evo update must use v3 ("bad-protx-version-disallowed").
- With V24, applying the tx to a state copy (`ApplyProUpServTx`/`SetStateVersion`) must succeed. A legacy→basic migration must not collide with an operator key under either scheme.
- No netInfo entry or platformNodeID may be held by another proTxHash.
- If `scriptOperatorPayout` is non-empty: `nOperatorReward` must be ≠0 and the script must be P2PKH or P2SH.
- `inputsHash`.
- BLS `sig.VerifyInsecure(state.pubKeyOperator, SerializeHash(payload))`. The hash excludes the sig.

##### 2.3 CProUpRegTx (type 3)
```
u16 nVersion; uint256 proTxHash; u16 nMode; BLSPubKey pubKeyOperator(legacy iff v1); CKeyID keyIDVoting;
v3: u8 count + payouts[] ; else CScript scriptPayout; uint256 inputsHash; if !GETHASH vector<u8> vchSig
```
**Rules:**
- Trivial: version; `nMode==0`; operator valid; voting non-null; legacy flag matches; payout list valid with `owner=null`.
- The MN must exist and must **not be shared** ("bad-protx-shared-mn").
- Version-downgrade rule as for ServTx.
- With V24: if the key changes or a scheme migration happens, `HasOperatorKeyUnderAnyScheme(newKey, self)` must be false. If the key does not change, `SetStateVersion` to max(state, tx) must succeed.
- `IsPayoutListTriviallyValid(payouts, state.keyIDOwner, newVoting)`.
- `IsPayoutListKeySafe` against the collateral coin's destination. Collateral reuse is checked only if `max(state, tx) ≥ 3`.
- The operator key must not belong to another MN.
- Pre-enforcement: owner == voting.
- `inputsHash`.
- **Sig**: `CHashSigner::VerifyHash(SerializeHash(payload), keyIDOwner, vchSig)`. This is compact ECDSA recovery; non-canonical signatures are allowed here.

##### 2.4 CProUpRevTx (type 4)
```
u16 nVersion; uint256 proTxHash; u16 nReason (0..3); uint256 inputsHash; if !GETHASH BLSSig sig
```
**Rules:** `nReason <= 3`; the MN exists; version-downgrade rule; `inputsHash`; BLS sig by the current operator key over `SerializeHash`.

##### 2.5 Shared masternode types (V24): 10, 11, 12

**CProDisTx (type 10)** (`providertx.h:395`): `u16 nVersion=1, uint256 proTxHash, u16 actorIndex, u8 sig_count, sig_count×65-byte sigs`. There is no inputsHash, and the sigs are always serialized, including for GETHASH.
- Trivial: version 1; `1 <= sigs <= 8`.
- `CheckProDisTx`: requires V24 ("bad-prodis-too-early"). The MN must exist at `pindexPrev`, so registering and dissolving in the same block is invalid.
- `CheckProDisTxForList(spendHeight = prev+1)` (`specialtxman.cpp:1645`):
  - The MN must be shared.
  - **Exactly one input**: it must equal the MN collateral outpoint, with an **empty scriptSig**.
  - `actorIndex < shares.size()`.
  - `sigs == 1` (unilateral) or `sigs == shares.size()` (unanimous).
  - `early = spendHeight - nRegisteredHeight < nEarlyPeriodBlocks`.
  - `P = (unanimous || !early) ? 0 : nEarlyPenalty`.
  - **Outputs**: exactly `N-1` or `N` outputs.
    - The first `N-1` go, in share order skipping the actor, to each non-actor's `scriptRefund` ("bad-prodis-payee").
    - Each bonus (`out.nValue - share.amount`) must be at least `ProRataFloor(P, amount_i, W)`, where W is the sum of non-actor amounts ("bad-prodis-penalty-floor").
    - An optional last output pays the actor's `scriptRefund` and must be non-zero ("bad-prodis-actor-output-zero").
    - `Σbonus >= P` ("bad-prodis-penalty-sum").
    - `fee = Σshares.amount - Σouts <= MAX_FEE = 1,000,000` duffs ("bad-prodis-fee").
    - Unilateral only: `Σbonus <= nEarlyPenalty` ("bad-prodis-bonus").
  - **Sig digest** (`MakeSignHash`): `SHA256d( ser(string "DashSharedMNDissolve") || u16 payload.nVersion || i32 tx.nVersion(16-bit field) || u16 tx.nType || u32 nLockTime || all prevouts || all nSequence (u32) || all CTxOut || proTxHash || u16 actorIndex || u8 sig_count )`.
    - Unanimous: each `sigs[i]` is verified against `shares[i].keyIDOwner`.
    - Unilateral: `sigs[0]` is verified against `shares[actor].keyIDOwner`.
    - All use `VerifyHashCanonical`: 65 bytes, header in 27..34, low‑S (`messagesigner.cpp:58`).
  - `tx.nVersion` is `int16_t` (`primitives/transaction.h:253`), so it is hashed as 2 bytes LE; `tx.nType` as u16 (verified).
  - Removal from the list happens in the generic collateral-spend loop (§4.3).

**CProUpShareTx (type 11):** `u16 nVersion=1, uint256 proTxHash, u16 shareIndex, CScript scriptReward, uint256 inputsHash, [!GETHASH] vector vchSig`.
- Trivial: `vchSig.size() == 65`; `scriptReward` is P2PKH or P2SH.
- Requires V24.
- The MN must exist and be shared, with the index in range.
- The reward destination must not be P2PKH(voting) or P2PKH of any share owner key.
- `inputsHash`.
- `VerifyHashCanonical(SerializeHash(payload), shares[idx].keyIDOwner)`.
- **Effect**: `shares[idx].scriptReward = scriptReward`. Setting it equal to the refund script is the "reset". `BuildNewList` rechecks `IsShareListVotingKeySafe` against the in-block state.

**CProUpSharedRegTx (type 12):** `u16 nVersion=1, uint256 proTxHash, BLSPubKey (always basic encoding), CKeyID keyIDVoting, uint256 inputsHash, [!GETHASH] u8 count + count×65-byte sigs`.
- Trivial: operator key valid and not legacy; voting non-null; `1..8` sigs.
- Requires V24; the MN must be shared.
- `sigs == shares.size()` (unanimity).
- The operator key must not be held by another MN under either scheme.
- The new voting key must not be any share's refund or reward destination.
- `inputsHash`.
- `sigs[i]` verified canonically over `SerializeHash(payload)` by `shares[i].keyIDOwner`.
- **Effect**: same as ProUpRegTx on an operator change (reset operator fields, PoSe-ban until a ProUpServTx). The voting key is set and the state version is restored.

##### 2.6 Payout list (DIP‑0026 multi-payout, v3)

`IsPayoutListTriviallyValid` (`providertx.cpp:25`):
- 1..8 entries.
- Each `reward` is in [100, 10000] and the sum is exactly 10000.
- Each script is P2PKH or P2SH.
- Scripts are unique.
- A destination must be extractable.
- No entry may pay P2PKH(owner, if non-null) or P2PKH(voting).

Legacy scriptPayout is treated as `[{scriptPayout, 10000}]` (`LegacyPayoutAsList`).

##### 2.7 Share table (shared MN)

`IsShareListTriviallyValid` (`providertx.cpp:63`), with `required = 1000 DASH`:
- 2..8 shares.
- `nEarlyPeriodBlocks <= 420480`.
- Each amount is in `[100 DASH, required]`, and the sum equals `required`.
- Owner keys are non-null and unique.
- Refund scripts are unique.
- Refund and non-empty reward scripts are P2PKH or P2SH.
- A refund or reward script may not pay P2PKH(voting) or P2PKH(any share owner key).
- `0 <= nEarlyPenalty < min(amount)`.
- If `nEarlyPeriodBlocks == 0`, then `nEarlyPenalty` must be 0.
- An empty `scriptReward` means "use scriptRefund".

**Consent digest** (`MakeSharedRegConsentHash`, `providertx.cpp:338`):
```
SHA256d( ser("DashSharedMNReg") || u16 payload.nVersion || tx.nVersion || tx.nType || tx.nLockTime ||
         CalcTxInputsHash(tx) || each vin.nSequence || CalcTxOutputsHash(tx) || payload.nType || nMode ||
         netInfo (ExtNetInfo when v3) || keyIDVoting || pubKeyOperator(legacy iff v1) || nOperatorReward ||
         u8 shares.size() || each share || nEarlyPeriodBlocks || nEarlyPenalty )
```

##### 2.8 Network fields (netinfo)

`CheckProviderNetworkFields` (`providertx.cpp:212`):
- netInfo must be non-null and `CanStorePlatform() == (v>=3)`.
- If empty: OK only if `allow_empty`.
- Otherwise the trivial rules apply, plus `Validate()`, whose status maps to the reject reasons "bad-protx-netinfo-addr/-port/-addr-type/-addr-unroutable/-bad/-entry/dup-netinfo-entry/-maxlimit".
- **Evo**: `platformNodeID` must be non-null.
  - v3: the scalar ports must be 0.
  - v<3:
    - On mainnet, p2pPort must be 26656 and httpPort 443.
    - On every network, neither may equal the mainnet core port 9999.
    - p2p ≠ http ≠ core port.

**`MnNetInfo`** (v1/v2):
- One CService, IPv4 only.
- Routable if `RequireRoutableExternalIP`.
- The port must be 9999 on mainnet and **must not** be 9999 on other networks.
- Serialized as a plain CService in ADDRv1 encoding (18 bytes).

**`ExtNetInfo`** (v3): `u8 version(=1)` then `std::map<u8 purpose, vector<NetInfoEntry>>`.
- **Purposes**: 0 CORE_P2P, 1 PLATFORM_P2P, 2 PLATFORM_HTTPS.
- **Entry encoding**: `u8 type` (0x01 Service → CService serialized with **ADDRv2** via OverrideStream; 0x02 Domain → `LIMITED_STRING(addr, 253)` plus a big-endian u16 port; 0xff Invalid).
- `Validate()`:
  - version must be 1 and the map non-empty.
  - No addr:port duplicates anywhere, and no address duplicates within a purpose.
  - Each list is non-empty with at most 4 entries.
  - **Services**: IPv4, IPv6, Tor, I2P, or CJDNS; routable if required. I2P must use port 0 (`I2P_SAM31_PORT`). Otherwise the port must be ≠0 and not `IsBadPort()`, except that 443 is allowed for PLATFORM_HTTPS.
  - **Domains** (PLATFORM_HTTPS only):
    - Lowercase only.
    - Length 3..253; labels 1..63 characters, not starting or ending with '-'; at least two labels; RFC1035 charset.
    - The TLD must be alphabetic and not in `.mail .arpa .example .invalid .localhost .test .local .corp .home .internal .intranet .lan .private`.
    - `.onion` and `.i2p` names are stored as CService instead.
- `GetPrimary` = the first CORE_P2P entry.

---

#### 3. Shared-collateral covenant (V24)

##### 3.1 Template script

`sharedcollateral.h`: `04 44 53 48 43 75 51` = push4 "DSHC", OP_DROP, OP_TRUE. It is anyone-can-spend at the script layer and matched by exact comparison only.

##### 3.2 Creation rule

`CheckSharedCollateralTemplateOutputs`, run for **every block tx including the coinbase**, in `ProcessSpecialTxsInBlock` when `is_v24_active`. Any output with the template script must be `vout[n]` of a shared ProRegTx with an internal collateral at index `n`. Otherwise the tx fails "bad-shared-collateral-create". Template outputs created before activation are permanently frozen.

##### 3.3 Spend rule

`CheckSharedCollateralSpends`, run in `ConnectBlock` after `CheckTxInputs` when `fV24Active_context = ActiveAt(*pindex)`, and in mempool. A non-coinbase input spending a template-script coin must belong to a type‑10 tx ("bad-shared-collateral-spend"). `BuildNewListFromBlock` also rejects a non-ProDisTx spend of a shared MN collateral.

##### 3.4 Reward split for shared MNs

In `GetBlockTxOuts`: `SplitAmountByShares(ownerReward)`. Every share except the last gets `floor(total*amount_i/1000 DASH)`, computed with 128-bit intermediates. The last share gets the remainder. Zero-value amounts are omitted. Each is paid to `RewardScript()`.

---

#### 4. Deterministic masternode list

##### 4.1 Data structures

**`CDeterministicMN`** (`deterministicmns.h`), serialized as:
`uint256 proTxHash, VARINT internalId, COutPoint collateralOutpoint, u16 nOperatorReward, CDeterministicMNState, u16 nType`.

`nType` is written if `s.GetVersion() == CLIENT_VERSION || >= 70227`; otherwise it reads as Regular.

**`internalId`** = `newList.GetTotalRegisteredCount()` at registration. `nTotalRegisteredCount = max(id+1, count)`. Neither is consensus; both can differ between nodes. Diffs reference MNs by internalId.

**`CDeterministicMNState`** (`dmnstate.h`) is serialized in this order:

| Field | Type | Notes |
|---|---|---|
| nVersion | int32 | ProTx version of the state |
| nRegisteredHeight | int | |
| nLastPaidHeight | int | |
| nConsecutivePayments | int | evonode 4-in-a-row counter |
| nPoSePenalty | int | |
| nPoSeRevivedHeight | int | -1 default |
| nPoSeBanHeight | int | -1 = not banned |
| nRevocationReason | u16 | |
| confirmedHash | uint256 | |
| confirmedHashWithProRegTxHash | uint256 | **single** SHA256(proTxHash‖confirmedHash) |
| keyIDOwner | CKeyID | null for shared MNs |
| pubKeyOperator | BLS (legacy iff v1) | |
| keyIDVoting | CKeyID | |
| netInfo | Mn/ExtNetInfo (Ext iff v≥3) | |
| v≥3: payouts, shares, nEarlyPeriodBlocks u32, nEarlyPenalty i64 | **std::vector → CompactSize counts** (unlike the u8 counts in payloads) | |
| v<3: scriptPayout | CScript | |
| scriptOperatorPayout | CScript | |
| platformNodeID | uint160 | always serialized |
| v<3: platformP2PPort, platformHTTPPort | u16,u16 | |

**`CDeterministicMNStateDiff`** is serialized as `VARINT(fields)` followed by the present fields **in the hana tuple order**:

`nVersion, nRegisteredHeight, nLastPaidHeight, nPoSePenalty, nPoSeRevivedHeight, nPoSeBanHeight, nRevocationReason, confirmedHash, confirmedHashWithProRegTxHash, keyIDOwner, pubKeyOperator, keyIDVoting, netInfo, scriptPayout, payouts, shares, nEarlyPeriodBlocks, nEarlyPenalty, scriptOperatorPayout, nConsecutivePayments, platformNodeID, platformP2PPort, platformHTTPPort`

That is not bit order. The bits are:

`nVersion 0x1, nRegisteredHeight 0x2, nLastPaidHeight 0x4, nPoSePenalty 0x8, nPoSeRevivedHeight 0x10, nPoSeBanHeight 0x20, nRevocationReason 0x40, confirmedHash 0x80, confirmedHashWithProRegTxHash 0x100, keyIDOwner 0x200, pubKeyOperator 0x400, keyIDVoting 0x800, netInfo 0x1000, scriptPayout 0x2000, scriptOperatorPayout 0x4000, nConsecutivePayments 0x8000, platformNodeID 0x10000, platformP2PPort 0x20000, platformHTTPPort 0x40000, payouts 0x80000, shares 0x100000, nEarlyPeriodBlocks 0x200000, nEarlyPenalty 0x400000`

Additional diff rules:
- If pubKeyOperator or netInfo is present, nVersion **must** also be present; otherwise deserialization throws. The diff builder forces it in.
- pubKeyOperator is compared by `GetHash()` (scheme-dependent), so a re-encoding is recorded as a change.
- The legacy diff format (key `dmn_D3`) put nVersion **last** (bit 0x40000) and shifted the other bits down by one. It is migrated to `dmn_D4` by `MigrateLegacyDiffs`.

**`CDeterministicMNListDiff`** is serialized as:
`vector<CDeterministicMN> addedMNs` (sorted by internalId), then `CompactSize n` with n × (`VARINT internalId` ascending, `StateDiff`), then `CompactSize m` with m × `VARINT internalId` (removed).

**`CDeterministicMNList`** is serialized as `blockHash, int nHeight, u32 nTotalRegisteredCount, CompactSize count, CDeterministicMN × count`. MNs are written in **immer HAMT iteration order**, hashed by `ReadLE64(proTxHash)`. `WriteDerived` byte-compares existing values, so byte-compatible evodb requires reproducing that order. The evo-snapshot codec sorts by proTxHash instead.

**Unique property map** (`mnUniquePropertyMap`): key is `SerializeHash(value, SER_GETHASH, PROTOCOL_VERSION [| ADDRV2_FORMAT if CService is not ADDRv1-compatible])`; value is `(proTxHash, refcount)`. Indexed values:
- `collateralOutpoint`
- every netInfo entry (CService or DomainPort)
- `keyIDOwner`, or each share's `keyIDOwner` (same namespace, so no cross reuse)
- `pubKeyOperator` (its serialization depends on the scheme, so a key can occupy one of two slots; hence `HasOperatorKeyUnderAnyScheme`)
- `platformNodeID` (Evo only)

`keyIDVoting` is **not** unique. Null values are never inserted. `AddMN`, `UpdateMN`, and `RemoveMN` throw on a conflict; in a block this surfaces as "failed-dmn-block" or "failed-procspectxsinblock".

##### 4.2 MN state transitions per block (`BuildNewListFromBlock` → `RebuildListFromBlock`, `specialtxman.cpp:322-796`)

The order of operations is consensus-relevant:

1. `newList = prevList`, with `blockHash = 0` and `height = prev+1`. `payee = prevList.GetMNPayee(pindexPrev)` is computed **before** any modification.
2. **Confirmation**: for every MN with a null `confirmedHash`, if `pindexPrev.nHeight - nRegisteredHeight >= nMasternodeMinimumConfirmations` (mainnet 15, other networks 1), call `UpdateConfirmedHash(proTxHash, pindexPrev.hash)`.
3. `DecreaseScores()`: every non-banned MN with penalty > 0 gets penalty −1.
4. Iterate txs `1..n` (skipping the coinbase), special txs only, in block order:
   - **ProRegTx**:
     - Build a new DMN: `internalId = totalRegisteredCount`, collateral = `(txid, n)` if internal.
     - Re-check the external collateral coin if `view.GetBestBlock()` is non-null.
     - If an existing MN uses the same collateral, **remove it** (replacement).
     - Reject duplicate netInfo entries, owner/share keys, or operator key against the list as rebuilt so far.
     - State is built from the payload, with `nRegisteredHeight = height`. An empty netInfo means `BanIfNotBanned(height)`.
     - V24: reject if the operator key is held under either scheme.
     - `AddMN`.
   - **ProUpServTx**: check netInfo duplicates against other proTxHashes; check the type matches; `ApplyProUpServTx`. Pre-V24 the state version is **unchanged**: netInfo, operator payout, and the Evo platform fields are simply overwritten. V24: `state.nVersion = max(cur, tx)` and `SetStateVersion`. If the MN is banned and the operator key, voting key, and (shared or owner) key are all set, then `Revive(height)`: penalty=0, banHeight=-1, revivedHeight=height. V24 includes a migration collision check.
   - **ProUpRegTx**:
     - Shared MNs are rejected.
     - `target = V24 ? max(old, tx) : (operatorChanged ? tx.nVersion : old)`.
     - On an operator change: `ResetOperatorFields()` (nVersion=1, null operator key, empty netInfo, empty operator payout, revocationReason=0, platformNodeID=0), then `BanIfNotBanned`, then set the new key.
     - Set voting; `SetStateVersion(target)`.
     - Payouts (target ≥3) or scriptPayout.
   - **ProUpRevTx**: `ResetOperatorFields()`. Pre-V24 the state version **drops to 1**. V24 keeps `max(old, tx)`. Then `BanIfNotBanned` and `nRevocationReason = reason`.
   - **ProUpShareTx / ProUpSharedRegTx**: as in §2.5.
   - **QcTx**: if the commitment is non-null, find the quorum base block and the members (`GetAllQuorumMembers`). For each member still in the list with `validMembers[i] == false`: `PoSePunish(CalcPenalty(66))`.
   - ProDisTx (type 10) does nothing in this loop.
5. **Second pass over txs 1..n**: any input whose prevout is an MN collateral calls `RemoveMN`. For a shared MN the spender must be type 10.
6. **Payee bookkeeping**: if `payee` still exists, `nLastPaidHeight = height`. If the payee is Evo and MN_RR is not active (`ActiveAfter(pindexPrev)`), increment `nConsecutivePayments`.
7. Reset `nConsecutivePayments` to 0 on every Evo MN that is not (the payee with MN_RR inactive) and has a non-zero value. After MN_RR this resets every Evo MN.

`SetStateVersion` (`specialtxman.cpp:51`):
- Migrates `scriptPayout` → `payouts` when the version is ≥3.
- Re-encodes the operator key with `Set(pubkey, v==1)`.
- Converts MnNetInfo ↔ ExtNetInfo by re-adding entries via `AddEntry(purpose, "addr:port")`. A failure is "bad-protx-netinfo-version".
- An Evo MN with legacy netInfo cannot be lifted to ExtAddr except by a ProUpServTx carrying platform entries.
- When lifting, the scalar platform ports are zeroed.

##### 4.3 PoSe

- `CalcMaxPoSePenalty() = max(100, total MN count)`.
- `CalcPenalty(p) = max*p/100`.
- **`PoSePunish`**: `penalty = min(max, penalty + p)`. If the MN was not already banned and penalty ≥ max, ban at the list's `nHeight`.
- A banned MN's penalty stops decaying. Revival is only by ProUpServTx (§4.2).
- The only on-chain punishment source is failed DKG membership in a mined QcTx: 66% of max.
- Platform PoSe bans (§11) are **not** on-chain. They only make the local MN vote "bad" in DKGs.

##### 4.4 Payee selection

`GetMNPayee(pindexPrev)` (`deterministicmns.cpp:150`):
- If V19 is active and MN_RR is not (both `ActiveAfter(pindexPrev)`): when a valid MN has `nLastPaidHeight == list.nHeight` (the previous block's payee), is Evo, and has `nConsecutivePayments < 4`, pay it again.
- Otherwise pick the minimum over **valid** (non-banned) MNs of `CompareByLastPaid`:
  - `h = nLastPaidHeight`.
  - If `nPoSeRevivedHeight != -1 && revived > h`, then `h = revived`.
  - Else if `h == 0`, then `h = nRegisteredHeight`.
  - Ties are broken by `proTxHash <` (byte-wise memcmp of the internal little-endian bytes, `base_blob::Compare`).
- `confirmedHash` is **not** required for payment.
- Projected payees (RPC): each Evo entry is replicated 4× pre-MN_RR.

##### 4.5 Persistence and manager

- `ProcessBlock` writes `("dmn_D4", blockHash) → diff(prevList → newList)` via `WriteDerived`. Every `nHeight % 576 == 0`, or the first block after the initial snapshot, it also writes `("dmn_S3", blockHash) → full list`.
- **Initial list**: the first DIP3 block with no diff on disk gets an empty list `(hash, height, 0)`.
- **Lookup**: walk back to a cached list or `dmn_S3` snapshot, then apply diffs forward. Mini-snapshots every 32 blocks are kept in memory only.
- **Undo**: reads the diff and drops the caches. The diff is not erased.
- `dmn_R1` is a "repaired" flag written by the `evodb repair` RPC.
- On ProUpServTx in a connected block, `CMasternodeMetaMan::ResetPlatformBan(proTxHash, height)` runs. This is non-consensus.

---

#### 5. Simplified MN list (SML) and mnlistdiff

##### 5.1 CSimplifiedMNListEntry (`simplifiedmns.h:52-89`)

- Network serialization, if peer protocol ≥ 70228: `u16 nVersion` is prepended. It is **not included in the hash**.
- Then: `proRegTxHash, confirmedHash, netInfo (Ext iff v≥3), pubKeyOperator(legacy iff v1), keyIDVoting, bool isValid (= !banned)`.
- On network streams with protocol < 70227, stop here.
- If `nVersion >= 2`: `u16 nType`. If Evo: (v<3: `u16 platformHTTPPort`) then `uint160 platformNodeID`.

**`CalcHash` = `SHA256d(ser(entry))` with SER_GETHASH and CLIENT_VERSION.** The type and platform fields are included for v≥2, and nVersion is excluded.

##### 5.2 merkleRootMNList

Take the entries for **all** MNs (including banned ones), sort by `proRegTxHash.Compare` (memcmp of raw bytes), and compute the Bitcoin `ComputeMerkleRoot` over the entry hashes. A mutated tree is "mutated-calc-cb-mnmerkleroot". The list used is the list **after** this block (`BuildNewListFromBlock`). The result must equal `cbTx.merkleRootMNList` ("bad-cbtx-mnmerkleroot").

##### 5.3 CSimplifiedMNListDiff (`mnlistdiff`, request `getmnlistd {baseBlockHash, blockHash}`)

**Serialization:**
- If proto ≥ 70229: `u16 nVersion (=1)` first.
- `baseBlockHash, blockHash, CPartialMerkleTree cbTxMerkleTree, CTransaction cbTx`.
- If 70225 ≤ proto < 70229: `nVersion` here.
- `vector<uint256> deletedMNs, vector<SMLEntry> mnList, vector<pair<u8 llmqType, uint256>> deletedQuorums, vector<CFinalCommitment> newQuorums`.
- If proto ≥ 70230: `map<CBLSSignature, set<u16>> quorumsCLSigs`.

**Construction** (`smldiff.cpp:BuildSimplifiedMNListDiff`):
- Both blocks must be on the active chain with base ≤ block. A null base means genesis, but the response echoes the null hash.
- **mnList**: MNs that are new, or whose SML entry differs (`extended` also compares payouts, shares, and payout scripts).
- **deletedMNs**: proTxHashes absent from the target list.
- **Quorums**: the set difference of `GetMinedAndActiveCommitmentsUntilBlock`.
- **quorumsCLSigs** (after V20): for each new quorum, the CbTx CL sig at ancestor height `quorumBaseHeight − quorumIndex − 8`, grouped by signature. A null sig is allowed.
- **cbTxMerkleTree**: partial merkle tree matching only the coinbase.

---

#### 6. CbTx (type 5), `evo/cbtx.h`

```
u16 nVersion; i32 nHeight; uint256 merkleRootMNList;
v>=2: uint256 merkleRootQuorums;
v>=3: COMPACTSIZE bestCLHeightDiff; BLSSig bestCLSignature (global scheme; basic post‑V19); i64 creditPoolBalance;
v>=4: uint256 merkleRootAssetUnlocks
```

**CheckCbTx:**
- Version must be in 1..4.
- `nHeight == prev+1`.
- DIP0008 `ActiveAt(prev)` requires v≥2.
- V20 `ActiveAfter(prev)` requires v≥3, and pre-V20 forbids v≥3.
- `V24 && V20` requires v4; otherwise v4 is forbidden.

**merkleRootQuorums** (`CalcCbTxMerkleRootQuorums`):
1. Take the hashes of all mined-and-active commitments as of `pindexPrev`. Each hash is `SerializeHash(CFinalCommitment)`.
2. Add the non-null commitments in this block:
   - Rotation-enabled type: set `indexed[type][quorumIndex] = hash`.
   - Otherwise: if the list for the type is already at `signingActiveQuorumCount`, pop the oldest, then push.
3. Append the indexed maps in quorumIndex order.
4. Per type, the count must be at most `signingActiveQuorumCount`.
5. Concatenate everything, **sort the hashes** (byte order), and take the Merkle root ("bad-cbtx-quorummerkleroot").

**bestCL** (`CheckCbTxBestChainlock`, v≥3):
- If the previous block's CbTx had a non-null CL: the current one must be non-null ("bad-cbtx-null-clsig"), and `diff <= prevDiff + 1` ("bad-cbtx-older-clsig").
- If non-null:
  - `diff < nHeight`.
  - CL height = `nHeight − diff − 1`, block = the ancestor at that height.
  - It must pass `VerifyChainLock` ("bad-cbtx-invalid-clsig").
- If null: `diff` must be 0.

**creditPoolBalance**: see §7.

**merkleRootAssetUnlocks** (v4): Merkle root (no mutation check) over `GetInstanceHash()` (the full-serialization hash) of every asset unlock with payload v≥2, in block order; null if there are none. It is checked in **CheckBlock** as `BLOCK_MUTATED` ("bad-cbtx-assetunlockmerkleroot"), not as consensus-invalid.

**When these checks run:** CbTx and the merkle-root checks run only when `fCheckCbTxMerkleRoots == fScriptChecks`, i.e. not below assumevalid. The rule "a CbTx is required after DIP3" is also enforced there ("bad-cbtx-version").

---

#### 7. Credit pool, asset lock, asset unlock

##### 7.1 AssetLock (type 8)

**Payload:** `u8 nVersion, vector<CTxOut> creditOutputs`.

**Rules:**
- Exactly one OP_RETURN output, whose script is exactly `6a00` (2 bytes), with value > 0 and in MoneyRange.
- Version is 1..2; v2 requires V24.
- `creditOutputs` is non-empty; each value > 0 and in range, with a running sum in range.
- v1: each credit output is P2PKH. v2: P2PKH or P2SH.
- `Σcredit == OP_RETURN value`.

**Policy only (non-standard):** more than 100 inputs; total size > 20480 bytes; payload v2.

##### 7.2 AssetUnlock (type 9)

**Payload** (fixed 145 bytes): `u8 nVersion, u64 index, u32 fee, u32 requestedHeight, uint256 quorumHash, BLSSig quorumSig`.

**Rules:**
- No inputs; at most 32 outputs.
- Version is 1..2; v2 requires V24.
- The index must not be in the pool's `indexes` (cumulative since genesis).
- `quorumHash` must be a known block.
- **Signature**: `msgHash = SerializeHash(tx with quorumSig zeroed)` (the full serialization). `requestId = SHA256d(ser("plwdtx") ‖ u64 index)`.
  - The quorum must be among the last `signingActiveQuorumCount + 1` quorums of `llmqTypePlatform` (mainnet LLMQ_100_67) at pindexPrev ("bad-assetunlock-too-old-quorum", TX_BAD_SPECIAL).
  - `prev.nHeight` must be in `[requestedHeight, requestedHeight+48)`.
  - `quorumSig.VerifyInsecure(quorumPubKey, SignHash(llmqType, quorumHash, requestId, msgHash))`.
- The tx fee is `payload.fee` (`CheckTxInputs`); it must be ≠0 and in range.

**v2 stable txid:** `GetHash()` is `SHA256d` of the serialization with the **last 132 bytes zeroed** (requestedHeight, quorumHash, quorumSig). The instance hash is the full serialization. The coinbase commits to it (§6). The p2p inventory type is `MSG_ASSET_UNLOCK = 34`.

##### 7.3 Credit pool (`evo/creditpool.cpp`)

**`CCreditPool` = {i64 locked, i64 currentLimit, i64 latelyUnlocked, CRangesSet indexes}.** The ranges are `std::set<{u64 begin, u64 end}>`.

**Construction for block B, from prev:**
- `credit_pool` = B's `CbTx.creditPoolBalance`.
- `unlocked` = Σ over B's unlocks of (`fee` + Σvout).
- Indexes are added and duplicates throw.
- distant = ancestor at `B.h − CreditPoolPeriodBlocks` (576 on mainnet, testnet, and devnet; 100 on regtest).
- `latelyUnlocked = prev.latelyUnlocked + unlocked − distant.unlocked`.
- `currentLimit` by era:
  - **V24** (`ActiveAt(B)`): `UnlockLimitV24(balance, distantBalance) = min(max(0, max(start*20/100, 2000 DASH) − (start − balance)), balance)`.
  - **WITHDRAWALS**: `min(balance, 2000 DASH)`. This is per block; `latelyUnlocked` is not subtracted.
  - **Earlier**: if `balance + latelyUnlocked > 100 DASH`, then `limit = max(100, balance/10) − latelyUnlocked`, clamped at ≥0. Then `limit = min(limit, 1000 − latelyUnlocked)`.
  - A negative limit throws.

**Validating block N** (`CheckCreditPoolDiffForBlock`, V20 and DIP8 `ActiveAt(N)`):
- Start from `pool(prev)`.
- `platformReward = MN_RR ActiveAfter(prev) ? PlatformShare(GetMasternodePayment(h, subsidy, EvoReward)) : 0`.
- Per tx: a lock adds its OP_RETURN value. An unlock adds `fee + Σvout`, and the running total must stay ≤ `pool.currentLimit` ("failed-creditpool-unlock-too-much"); the index must be new.
- `cbTx.creditPoolBalance` must equal `pool.locked + Σlock − Σunlock + platformReward` ("bad-cbtx-assetlocked-amount").

**DB:** a snapshot `("cpm_S", blockHash) → CCreditPool` every 576 heights. Otherwise the pool is recomputed by reading blocks.

---

#### 8. MNHF / EHF (type 7, `evo/mnhftx.cpp`)

**Payload:** `u8 nVersion(=1)`, then `MNHFTx{u8 versionBit, uint256 quorumHash, BLSSig sig (always basic)}`.

**Check:**
- Version 1; `bit < VERSIONBITS_NUM_BITS` (29).
- `IsValidMNActivation(bit, prev.MTP)`: the deployment with that bit must have `useEHF` if its time window contains MTP. Unknown bits pass, with a warning.
- `quorumHash` must be a block on prev's chain.
- `msgHash = GetHash()` of the tx with `sig` set to null (96 zero bytes). The signer builds the tx with `PrepareTx()`: v3, type 7, no vin/vout, lock 0.
- `requestId = SHA256d(ser("mnhf") ‖ i64 bit)`.
- The sig is verified against a quorum of `llmqTypeMnhf` (mainnet LLMQ_400_85) using `SignHash(type, quorumHash, requestId, msgHash)`.

**Block processing:**
- No duplicate bits within a block.
- A bit must not already be in `signals`.
- `IsValidMNActivation(bit, block.MTP)` is required.
- `signals` is a `map<u8 bit, int minedHeight>`, written as `("mnhf_s2", blockHash)` after V20. `"mnhf_s"` is the legacy key, read only before MN_RR.
- `GetSignalsStage(prev)` drops signals whose mined block's MTP is earlier than the deployment's `nStartTime` (bit reuse).

**Effect on versionbits:** for `useEHF` deployments, `SignalHeight = signal mined height` (∞ if absent). DEFINED→STARTED requires `MTP ≥ start && prev.h ≥ signalHeight`. The threshold decays as `max(thMin, thStart − attempt²·period/100/falloff)` (`versionbits.cpp:256`).

**MN side** (`llmq/ehf_signals.cpp`): on each tip, for every `useEHF` deployment inside its window and not yet signalled, ask the quorum to sign. When the recovered sig arrives, build the type‑7 tx and add it to the mempool.

---

#### 9. Masternode payments and block reward (`masternode/payments.cpp`)

##### 9.1 Subsidy

`GetBlockSubsidyHelper` (`validation.cpp:1810`):
- After V20 the base is fixed at 5 DASH. Before that it was difficulty-based.
- Yearly decay: subtract `nSubsidy/14` every `nSubsidyHalvingInterval` = 210240 blocks.
- **Treasury part** = `subsidy/(V20 ? 5 : 10)` after `nBudgetPaymentsStartBlock`. `GetBlockSubsidy` returns the subsidy *minus* the treasury part.

##### 9.2 MN share: `GetMasternodePayment(h, value, era)`

- Starts at 20%, plus the historical steps (to 50% at 313520).
- From the first superblock cycle after BRR: `value * period[min((h − reallocStart)/(3*cycle), 18)] / 1000`, using periods 513…600.
- **Era ≠ Classic (V20 or later): `value*3/4`.** Note that the CreditPool era also returns 75%.
- Era is set by `GetMnRewardEraAfter(prev)`: Classic, then CreditPool (V20), then EvoReward (MN_RR).

##### 9.3 Expected coinbase outputs (`GetBlockTxOuts`)

1. `mnReward = GetMasternodePayment(h, subsidy + fees, era)`.
2. If era is EvoReward: `platform = PlatformShare(GetMasternodePayment(h, subsidy, era)) = x*375/1000`. Then `mnReward -= platform`, and output `{platform, OP_RETURN}` (script `6a`).
3. If the list is empty, return OK. If the payee is null, the call fails. **`IsTransactionValid` then returns true**, so any coinbase is accepted for MN payments. This is a pitfall.
4. Operator: if `nOperatorReward != 0` and `scriptOperatorPayout` is non-empty, `op = mnReward*nOperatorReward/10000` and `mnReward -= op`.
5. **Owner outputs:**
   - Shared: split by shares (§3.4).
   - Otherwise, for each payout except the last: `mnReward*reward/10000`. The last gets the remainder. Zero amounts are omitted.
6. The operator output comes last.

**Validation:** each expected output must appear in the coinbase. Pre-V24 this is an existence check. **With V24, strict multiset matching** is used (`FindUnmatchedMasternodePayment`). Payees are checked only after DIP3 enforcement.

##### 9.4 Block value

`IsBlockValueValid`:
- Coinbase out ≤ `subsidy + fees`.
- At superblock heights, out ≤ that plus `GetPaymentsLimit = superblockPart(nBits=1 or powLimit) × nSuperblockCycle`.
- Exact superblock payee checks happen only if governance data is valid, the node is synced, and the block is not chainlocked (`IsSuperblockValidationRequired`). Otherwise they are skipped (`SuperBlockCheckType::NoCheck`).
- V24 disallows duplicate superblock outputs.

These checks are not purely deterministic. A Rust node should treat superblock-trigger validation as governance-dependent, like Core.

---

#### 10. evodb (`evo/evodb.*`)

- **Storage:** LevelDB at `<datadir>/evodb` (`GetDataDirNet()`), 64 MiB cache.
- **Transactions:** a two-level design, `CDBTransaction<CDBWrapper>` (root) wrapping `CDBTransaction<root>` (current).
  - Each block connect/disconnect runs inside `BeginTransaction()`; the scoped committer commits the current transaction into the root, or rolls back.
  - `CommitRootTransaction()` writes the batch at chainstate flush (`validation.cpp:3095`). The current transaction must be clean.
- **Identities** NORMAL and SNAPSHOT exist for assumeutxo dual chainstates.
- **`WriteDerived`** refuses to overwrite with different bytes, which signals local corruption and aborts the node.

| Key (string, then tuple fields) | Owner | Value |
|---|---|---|
| `"b_b4"` | evodb | best block hash (NORMAL) |
| `("b_b4", u8 1)` | evodb | best block (SNAPSHOT identity) |
| `"b_dcs"`, `"b_dcs_mn"`, `"b_dcs_bg_mn"` | evodb | dual-chainstate markers and MN-list hashes |
| `("dmn_S3", blockHash)` | DMN | full `CDeterministicMNList`, every 576 blocks |
| `("dmn_D4", blockHash)` | DMN | `CDeterministicMNListDiff`, every block |
| `("dmn_D3", blockHash)` | DMN | legacy diff format (migrated) |
| `"dmn_R1"` | DMN | repaired flag |
| `("cpm_S", blockHash)` | credit pool | `CCreditPool`, every 576 |
| `("mnhf_s2", blockHash)` / `("mnhf_s", …)` | MNHF | `map<u8,int>` |
| `("q_mc", (u8 llmqType, quorumHash))` | llmq blockprocessor | `pair<CFinalCommitment, minedBlockHash>` |
| `("q_mcih", u8 type, BE32(UINT32_MAX − minedHeight))` | llmq | quorum base height |
| `("q_mcihi", u8 type, int quorumIndex, BE32(…))` | llmq | indexed variant |
| `"q_bbu2"` | llmq | upgrade best block |
| `("llmq_S", SerializeHash(pair(u8 type, blockHash)))` | llmq snapshot | `CQuorumSnapshot` |

Separate LevelDBs, outside this scope, live under `<datadir>/llmq/`: `dkgdb` (`qdkg_V/S/E`), `isdb` (`is_*`), `recsigdb` (`rs_*`), `quorumdb` (`q_Qsk`, `q_Qqvvec`).

---

#### 11. Platform-related consensus and p2p

- **Evo fields:** `platformNodeID` (uint160) must be unique and non-null. The ports are scalars in v2 and ExtNetInfo purposes 1 and 2 in v3. Collateral is 4000 DASH; voting weight 4.
- **PLATFORMBAN** (`net_processing.cpp:3835`): `PlatformBanMessage{uint256 protx, i32 requestedHeight, uint256 quorumHash, [!GETHASH] BLSSig(basic)}`. Inventory hash = `SerializeHash` without the sig; `MSG_PLATFORM_BAN = 33`; v2 short id 168 (proto ≥ 70240). Acceptance requires all of:
  - The target is an Evo MN.
  - `tip−576 ≤ requestedHeight ≤ tip`.
  - A `llmqTypePlatform` quorum exists.
  - The sig is valid over `SignHash(type, q, SHA256d(ser("PlatformPoSeBan") ‖ protx ‖ i32 h), SHA256d(protx ‖ i32 h))`.
  - Then: store in meta (`m_platform_ban`) and relay.
- **Effect** (non-consensus): the local MN marks the target a bad DKG member, which leads to a failed DKG and then an on-chain PoSe penalty. The ban clears on that MN's next ProUpServTx.
- **Credit pool platform reward** (§7, §9) and asset lock/unlock are the on-chain Platform hooks.

---

#### 12. Active masternode (`active/masternode.cpp`, `active/context.*`)

- **Enabled by** `-masternodeblsprivkey=<hex>` (32-byte BLS secret).
- **Parameter interactions:** `-disablewallet=1`, `-peerblockfilters=1`, `-blockfilterindex=basic`.
- **Hard requirements:** `-listen` (when a routable IP is required), `-txindex=1`, `-peerbloomfilters=1`, no pruning, `maxconnections ≥ default`, governance enabled, not blocksonly.
- **States:** WAITING_FOR_PROTX, POSE_BANNED, REMOVED, OPERATOR_KEY_CHANGED, PROTX_IP_CHANGED, READY, ERROR.
- **Init** (after DIP3 enforcement):
  - Local address: `GetLocal` toward 8.8.8.8 (IPv4 only, routable on main/test). Fallbacks are 127.0.0.1 on regtest/devnet, or the address peers see.
  - Find the MN with `GetMNByOperatorKey(pk)`.
  - The MN must be valid, and its primary netInfo must equal the local address.
  - A self-connect test is done; only routable networks require it to succeed.
  - Then READY.
- **On each tip:** reset if removed, if the operator key changed, or if the primary IP changed.
- **Roles via ActiveContext:** DKG sessions (`CDKGSessionManager`), sig shares, ChainLock signer, InstantSend signer, governance signer, EHF signer, CoinJoin server.
- **DKG "badConnection" criteria** (`active/dkgsession.cpp:255-287`), applied to other members:
  - not connected (when SPORK_21 applies)
  - proto < 70242
  - an MN connection that does not relay txs or lacks `NODE_COMPACT_FILTERS`
  - more than 5 failed outbound attempts
  - Platform-banned

**MNAUTH** (`evo/mnauth.cpp`):
- Sent right after VERACK to a peer that sent an `mnauth` challenge in VERSION.
- Format: `{uint256 proRegTxHash, BLSSig sig}`, signed with the basic scheme over `SerializeHash(tuple(operatorPubKey, peer's challenge, bool IsInbound, int ourProtoVersion))`.
- The receiver uses `!IsInbound` and `peer.nVersion`.
- **Receiver rules** (only after the blockchain is synced):
  - A duplicate MNAUTH is penalized 100.
  - The peer's services must include NETWORK|BLOOM (100).
  - A null hash or invalid sig is 100.
  - An unknown MN is 10; a bad sig is 10.
  - Duplicate verified connections are deduplicated via `DeterministicOutboundConnection`.
  - Quorum relay members are sent `qsendrecsigs`.
- Peers are disconnected when their MN is removed or its operator key changes.

---

#### 13. Masternode sync, meta, netfulfilled

- **`CMasternodeSync`** (`masternode/sync.h`): assets `BLOCKCHAIN=1 → GOVERNANCE=4 → FINISHED=999`. Constants 10 and 11 are vestigial.
  - Tick 6 s; timeout 30 s; reset after 900 s without a tip.
  - `IsBlockchainSynced()` is `asset > 1`.
  - **Tick** (`node/sync_manager.cpp`):
    - Send `getsporks` once per peer.
    - In BLOCKCHAIN: once the best header is reached and nothing new has arrived for 6 s (with more than 3 peers) or 30 s, switch assets and send `mempool` to outbound peers.
    - In GOVERNANCE: sync governance objects until timeout, then FINISHED.
    - The peer is disconnected once "full-sync" is fulfilled.
- **`mncache.dat`** (`CFlatDB`):
  - File layout: `ser(string "magicMasternodeCache") ‖ 4-byte network magic ‖ payload ‖ SHA256d(all preceding)`.
  - Payload: `string "CMasternodeMetaMan-Version-5"`, `vector<CMasternodeMetaInfo>`, `i64 nDsqCount`, `vector<uint256> usedMasternodes`.
  - `CMasternodeMetaInfo` = `{protx, i64 lastDsq, int mixingTxCount, map<uint256,int> govVotes, int outboundAttemptCount, i64 lastOutboundAttempt, i64 lastOutboundSuccess, bool platformBan, int platformBanUpdated}`.
  - A version mismatch discards the contents.
- **`netfulfilled.dat`** (`CFlatDB`, magic "magicFulfilledCache"): `map<CService, map<string, i64 expiry>>`. Expiry is 1 h on mainnet, 5 min elsewhere. Keys include "spork-sync", "full-sync", "mempool-sync", "allow-sync-<ms>", and governance-sync keys.

---

#### 14. Sporks (`spork.h` / `spork.cpp`)

| ID | Name | Default (all networks) | Meaning | Mainnet value (hardcoded) |
|---|---|---|---|---|
| 10001 | SPORK_2_INSTANTSEND_ENABLED | 4070908800 (OFF) | IS enabled when active; signing only when value == 0 | 0 |
| 10016 | SPORK_17_QUORUM_DKG_ENABLED | OFF | DKG on | 0 |
| 10018 | SPORK_19_CHAINLOCKS_ENABLED | OFF | CL enforcement; signing when value == 0 | 0 |
| 10020 | SPORK_21_QUORUM_ALL_CONNECTED | OFF | 0 = all quorum types fully connected; 1 = all except 100_67, 400_60, 400_85 | **1** |
| 10022 | SPORK_23_QUORUM_POSE | OFF | same EvalSpork semantics for DKG PoSe | 0 |
| 10023 | SPORK_24 | deprecated (do not reuse) | — | — |

- **Hardening** (`GetSporkValue`): if `!IsTestChain()` (mainnet), the spork table is ignored. All sporks return 0, except SPORK_21 which returns 1.
- `IsSporkActive(id) = value < adjustedNow`.
- **Message format:** `i32 nSporkID, i64 nValue, i64 nTimeSigned, LIMITED_VECTOR(vchSig, 65)`.
  - `GetHash` = `SHA256d` of all fields including the sig.
- **Signature:**
  - Testnet: compact ECDSA over `SHA256d(i32 id ‖ i64 value ‖ i64 time)`.
  - Other networks: signmessage of the decimal string `ToString(id)+ToString(value)+ToString(time)`, using the magic "DarkCoin Signed Message:\n".
  - The recovered key must equal `sporkPubKeyID`.
  - Valid only if `nTimeSigned ≤ adjustedNow + 2h`.
  - An invalid spork is penalized 100.
  - A newer `nTimeSigned` replaces the old one, which is then relayed.
- **Single signing address** (no multisig threshold any more), `chainparams.cpp`:
  - mainnet `Xgtyuk76vhuFW2iT7UAiHgNdWXCf3J34wh`
  - testnet and devnet `yjPtiKh2uwk3bDutTEA2q9mCtXyiZRWn55`
  - regtest `yj949n1UH6fDhw6HtVE5VMj2iSTaSWBMcW`
  - `-sporkaddr` overrides on regtest/devnet; `-sporkkey` enables signing.
- **P2P:** `spork`, `getsporks`; inventory `MSG_SPORK = 6`.
- **Persistence:** `sporks.dat` (`CFlatDB`, magic "magicSporkCache", payload `"CSporkManager-Version-3" ‖ map<uint256,CSporkMessage> ‖ unordered_map<SporkId,CSporkMessage>`). Re-verified on load.

---

#### 15. Gaps in the sibling rust-dashcore (`dash/src/blockdata/transaction/special_transaction/`)

- **Missing transaction types:** 10, 11, 12.
- **ProTx:** no v3 ExtAddr layout (u8-counted payouts and shares, join sigs, ExtNetInfo). `provider_registration.rs` always encodes the platform ports for Evo.
- **CbTx:** no v4 `merkleRootAssetUnlocks`.
- **AssetLock / AssetUnlock:** no v2 handling, and no stable-txid hashing for unlock v2.
- **Already present:** `sml/masternode_list_entry/net_info.rs` exists, so part of the ExtNetInfo work is done.

---

#### 16. Notes for a Rust reimplementation (pitfalls)

1. **"Active at" vs "active after".** Most gates use `ActiveAfter(pindexPrev)`, which means active for this block. These do not:
   - CbTx DIP8: `ActiveAt(*pindexPrev)`.
   - Credit pool, MNHF cache: `ActiveAt(*pindex)`.
   - Shared-collateral spend check: `ActiveAt(*pindex)`.
   - Creation check uses `is_v24_active = ActiveAfter(prev)`.

   Copy each one exactly.
2. **The BLS scheme is global state.** `CBLSSignature`/`CBLSPublicKey` default serialization follows `bls_legacy_scheme`, which flips to basic from the block *after* V19 activation and flips back on undo. This affects CbTx `bestCLSignature`, QcTx, MNAUTH, and others. Lazy keys carry their own flag, and ProTx and state use explicit version wrappers. The unique-property hash of an operator key depends on the scheme.
3. **Count encodings differ.** Payloads use **u8** counts for payouts, shares, and sigs. State and diff use **CompactSize** vectors. The diff uses VARINT for fields and internalIds. CbTx uses COMPACTSIZE for `bestCLHeightDiff`.
4. **Hash exclusions.** ProTx `SerializeHash` uses SER_GETHASH, which omits `vchSig`/`sig` (and the sig list for type 12). Type 10's sigs are **not** omitted, but they are not signed over either (separate digest). The SML entry hash omits nVersion but includes the v2+ type and platform fields.
5. **uint256 ordering** for SML sort, payee tie-break, and quorum-root sort is memcmp of the internal little-endian bytes, not numeric order.
6. **Unique-property hashing** for CService uses ADDRv2 serialization only for non-ADDRv1-compatible addresses (Tor v3, I2P, CJDNS). Entries are reference-counted.
7. **Order of list mutation within a block** matters: confirm, then decay, then special txs in order, then collateral spends, then payee bump, then consecutive-payment reset. The payee is computed from the previous list *before* the mutations and is still paid if it was removed in this block.
8. **No-payee leniency:** if no valid payee exists, `IsTransactionValid` returns true, so MN payment outputs are not enforced.
9. **assumevalid:** CbTx merkle roots, best-CL checks, and ProTx signatures are skipped when `fScriptChecks` is false. The DMN list is still built.
10. **Version transitions:** pre-V24, a ProUpRevTx downgrades the state to v1 and a ProUpServTx keeps the state version. Post-V24 versions only go up, via `SetStateVersion`, which re-encodes the key and converts netInfo (and can fail with "bad-protx-netinfo-version").
11. **evodb byte compatibility:** full-list snapshots serialize in immer HAMT iteration order. If you need to read or write Core's evodb, reproduce that order; otherwise define your own format and treat Core's as import-only.
12. **Shared MNs:**
    - A ProDisTx needs an empty scriptSig and canonical sigs (header 27..34, low‑S).
    - The penalty applies only to unilateral dissolutions inside the early period.
    - The fee comes out of the actor's share and is capped at 0.01 DASH.
    - A same-block register-then-dissolve or register-then-update is invalid because the MN must exist at prev.
    - Share owner keys share a namespace with `keyIDOwner`.
13. **Asset unlock v2 txid** zeroes the trailing 132 payload bytes. A block whose instance-hash root mismatches is "mutated", not invalid, so do not mark it permanently invalid.
14. **Credit-pool limit eras:** post-WITHDRAWALS (pre-V24) the limit is per block, `min(balance, 2000)`, not windowed. The V24 rule needs the CbTx balance at `h−576`. The index set is cumulative and must persist (as a range set).
15. **Sporks:** mainnet ignores spork messages for consensus-adjacent behaviour (hardcoded values). Testnet uses a different signature scheme (hash vs signmessage).
16. **Platform ports (v2 Evo):** fixed on mainnet (26656/443). On other networks the only rule is "≠9999 and distinct". `MnNetInfo` requires port 9999 on mainnet and forbids 9999 elsewhere.

### A.4 LLMQ (DKG, commitments, quorums, signing, rotation), ChainLocks, InstantSend, BLS

*Section numbers below (§1, §2, …) are local to A.4.*

This covers `src/llmq/`, `src/chainlock/`, `src/instantsend/`, `src/bls/` and `src/dashbls/` in `/work/home/agent/workspace/dash`, branch develop at `15db4787` (2026‑10‑06). Some files outside that scope (`src/active/`, `src/evo/`, `src/consensus/`) are included where the in‑scope code depends on them. Nothing was modified.

Two things in the brief do not exist in this tree:
- There is no non‑rotated `llmqTypeInstantSend`.
- There is no `llmqTypeAssetLocks`. Asset unlocks use `llmqTypePlatform`.

Notation used throughout:
- `H(x)` means `::SerializeHash(x)`, which is double‑SHA256 of the Bitcoin serialization. `CHashWriter` is also double‑SHA256.
- `uint256` serializes as its 32 raw internal bytes. Those bytes are little‑endian relative to the displayed hex.
- An `LLMQType` serializes as one `uint8`.
- `std::string` and `std::string_view` serialize as a CompactSize length followed by the bytes.

---

#### 0. File inventory

| Path | LOC | Role | Consensus? | Persistent state |
|---|---|---|---|---|
| src/llmq/params.h | 520 | LLMQType enum, `LLMQParams`, `available_llmqs[14]` | YES | – |
| src/llmq/options.{h,cpp} | 59/136 | spork evaluation, `IsQuorumRotationEnabled`, `-llmq-qvvec-sync` parsing, enabled types | YES (rotation gate) | – |
| src/llmq/utils.{h,cpp} | 87/856 | member selection (scores, modifier, quarter rotation, snapshots), connection/relay topology, `BlsCheck` | YES | writes snapshots via qsnapman |
| src/llmq/snapshot.{h,cpp} | 254/330 | `CQuorumSnapshot`, `CQuorumRotationInfo` (qrinfo), `CQuorumSnapshotManager` | YES (snapshots feed member selection) | evodb raw `llmq_S` |
| src/llmq/commitment.{h,cpp} | 173/325 | `CFinalCommitment`, `CFinalCommitmentTxPayload`, `CheckLLMQCommitment`, `BuildCommitmentHash` | YES | – |
| src/llmq/blockprocessor.{h,cpp} | 162/960 | qc mining and validation per block, mined‑commitment index, `qfcommit` handling, mineable pool | YES | evodb `q_mc`, `q_mcih`, `q_mcihi`, `q_bbu2` |
| src/llmq/dkgmessages.h | 188 | qcontrib, qcomplaint, qjustify, qpcommit structs | P2P wire | – |
| src/llmq/dkgsession.{h,cpp} | 269/773 | base DKG session (PreVerify/Receive for all phases) | MN protocol | – |
| src/active/dkgsession.{h,cpp} | 68/750 | `ActiveDKGSession`: contribute, complain, justify, commit, finalize | MN protocol | – |
| src/llmq/dkgsessionhandler.{h,cpp} | 118/92 | `QuorumPhase` enum, pending message queues | MN | – |
| src/active/dkgsessionhandler.{h,cpp} | 128/273 | phase clock (`UpdatedBlockTip`), sleeps | MN | – |
| src/llmq/dkgsessionmgr.{h,cpp} | 162/283 | handlers per (type, quorumIndex), contribution DB | MN | leveldb `llmq/dkgdb`: `qdkg_V`, `qdkg_S`, `qdkg_E` |
| src/llmq/net_dkg.{h,cpp} | 156/946 | DKG P2P intake, wire framing, batched sig verify, round driver | MN | – |
| src/llmq/debug.{h,cpp} | 134/231 | `quorum dkgstatus` (local only, no P2P) | no | – |
| src/llmq/quorums.{h,cpp} | 248/235 | `CQuorum`, `CQuorumDataRequest`, `MakeQuorumKey` | partial | `llmq/quorumdb`: `q_Qqvvec`, `q_Qsk` |
| src/llmq/quorumsman.{h,cpp} | 257/856 | `CQuorumManager` (ScanQuorums/ScanCommitments/GetQuorum), `SelectQuorumForSigning`, `VerifyRecoveredSig` | YES (selection and verification) | quorumdb, migration from evodb |
| src/llmq/net_quorum.{h,cpp} | 134/834 | qgetdata/qdata, data recovery threads, `EnsureQuorumConnections` | no | – |
| src/llmq/signhash.{h,cpp} | 76/24 | `SignHash` | YES | – |
| src/llmq/signing.{h,cpp} | 291/714 | `CRecoveredSig`, `CRecoveredSigsDb`, `CSigningManager` | partial | leveldb `llmq/recsigdb`: `rs_*` |
| src/llmq/signing_shares.{h,cpp} | 586/1671 | sig‑share sessions and protocol, recovery | MN | – |
| src/llmq/net_signing.{h,cpp} | 99/463 | qsigrec/qsigshare/qsigsesann/qsigsinv/qgetsigs/qbsigs handling, worker threads | P2P | – |
| src/llmq/ehf_signals.{h,cpp} | 54/140 | DIP‑23 EHF signal signing | MN | – |
| src/llmq/quorumproofs*.{h,cpp}, quorumproofdata.h | 101/748/32 | light‑client quorum proof chains (`getquorumproofchain` RPC) | no (RPC) | memo caches |
| src/llmq/cache.h, context.{h,cpp}, observer.{h,cpp}, types.h, core_write.cpp | small | per‑type LRU caches, wiring, JSON | no | – |
| src/chainlock/clsig.{h,cpp} | 73/271 | request id, `VerifyChainLock`, historical coinbase CL reader | YES | in‑mem caches |
| src/chainlock/chainlock.{h,cpp} | 89/174 | `ChainLockSig`, `Chainlocks` (best CL, HasChainLock/HasConflictingChainLock) | YES (via validation) | none (memory only) |
| src/chainlock/handler.{h,cpp} | 115/381 | CLSIG processing, enforcement, coinbase CL ingestion | YES‑ish | – |
| src/chainlock/signing.{h,cpp} | 95/322 | MN CL signer | MN | – |
| src/instantsend/lock.{h,cpp} | 72/63 | `InstantSendLock` (isdlock), request ids, `GetLockInputs` | P2P + validation | – |
| src/instantsend/db.{h,cpp} | 165/389 | IS DB | – | leveldb `llmq/isdb`: `is_*` |
| src/instantsend/instantsend.{h,cpp} | 172/542 | `CInstantSendManager` | partial | – |
| src/instantsend/net_instantsend.{h,cpp} | 125/674 | isdlock P2P, batch verification, block conflict resolution | partial | – |
| src/instantsend/signing.{h,cpp} | 117/475 | MN IS signer (inlocks, then islock) | MN | – |
| src/bls/bls.{h,cpp} | 602/439 | `CBLSWrapper`, Id, SecretKey, PublicKey, Signature, Lazy wrappers, global scheme flag | YES | – |
| src/bls/bls_ies.{h,cpp} | 120/109 | BLS‑IES (ECDH + AES‑256‑CBC) | MN protocol | – |
| src/bls/bls_worker.{h,cpp} | 195/891 | thread pool: contributions, vvec aggregation, share verify, caches | MN | – |
| src/bls/bls_batchverifier.h | 232 | batched verification by source and message | – | – |
| src/dashbls/ | vendored | dashpay/bls‑signatures, `AC_INIT([libdashbls],[2.1])` (configure.ac:2), bundled relic `RLC_VERSION 0.5.0` (configure.ac:133), updated to upstream `dbd61e6a` (commit 43861a1951) | YES | – |

---

#### 1. LLMQ types (src/llmq/params.h)

`MAX_LLMQ_SIZE = 400` (params.h:17). This bounds sig‑share inventories and batches. The size overrides reject anything larger.

`LLMQParams` fields, in order:
- `type`, `name`
- `useRotation`, `size`, `minSize`, `threshold`
- `dkgInterval`, `dkgPhaseBlocks`, `dkgMiningWindowStart`, `dkgMiningWindowEnd`, `dkgBadVotesThreshold`
- `signingActiveQuorumCount`, `keepOldConnections`, `keepOldKeys`, `recoveryMembers`

Derived values:
- `max_cycles(n) = useRotation ? n / signingActiveQuorumCount : n`
- `max_store_depth = max_cycles(keepOldKeys) * dkgInterval`
- `is_single_member = (size == 1)`

| Enum (u8) | name | rot | size | minSize | thr | dkgInterval | phaseBlk | mineStart | mineEnd | badVotes | activeCnt | keepOldConn | keepOldKeys | recoveryMembers |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 1 LLMQ_50_60 | llmq_50_60 | no | 50 | 40 | 30 | 24 | 2 | 10 | 18 | 40 | 24 | 25 | 48 | 25 |
| 2 LLMQ_400_60 | llmq_400_60 | no | 400 | 300 | 240 | 288 | 4 | 20 | 28 | 300 | 4 | 5 | 8 | 100 |
| 3 LLMQ_400_85 | llmq_400_85 | no | 400 | 350 | 340 | 576 | 4 | 20 | 48 | 300 | 4 | 5 | 8 | 100 |
| 4 LLMQ_100_67 | llmq_100_67 | no | 100 | 80 | 67 | 24 | 2 | 10 | 18 | 80 | 24 | 25 | 1440 | 50 |
| 5 LLMQ_60_75 | llmq_60_75 | YES | 60 | 50 | 45 | 288 | 2 | 42 | 50 | 48 | 32 | 64 | 64 | 25 |
| 6 LLMQ_25_67 | llmq_25_67 | no | 25 | 22 | 17 | 24 | 2 | 10 | 18 | 22 | 24 | 25 | 1440 | 12 |
| 100 LLMQ_TEST | llmq_test | no | 3 | 2 | 2 | 24 | 2 | 10 | 18 | 2 | 2 | 3 | 4 | 3 |
| 101 LLMQ_DEVNET | llmq_devnet | no | 12 | 7 | 6 | 24 | 2 | 10 | 18 | 7 | 4 | 5 | 8 | 6 |
| 102 LLMQ_TEST_V17 | llmq_test_v17 | no | 3 | 2 | 2 | 24 | 2 | 10 | 18 | 2 | 2 | 3 | 4 | 3 |
| 103 LLMQ_TEST_DIP0024 | llmq_test_dip0024 | YES | 4 | 4 | 3 | 24 | 2 | 12 | 20 | 2 | 2 | 4 | 4 | 3 |
| 104 LLMQ_TEST_INSTANTSEND | llmq_test_instantsend | no | 3 | 2 | 2 | 24 | 2 | 10 | 18 | 2 | 2 | 3 | 4 | 3 |
| 105 LLMQ_DEVNET_DIP0024 | llmq_devnet_dip0024 | YES | 8 | 6 | 4 | 48 | 2 | 12 | 20 | 7 | 2 | 4 | 4 | 4 |
| 106 LLMQ_TEST_PLATFORM | llmq_test_platform | no | 3 | 2 | 2 | 24 | 2 | 10 | 18 | 2 | 2 | 4 | 1440 | 3 |
| 107 LLMQ_DEVNET_PLATFORM | llmq_devnet_platform | no | 12 | 9 | 8 | 24 | 2 | 10 | 18 | 7 | 4 | 5 | 1440 | 6 |
| 0xff LLMQ_NONE | – | | | | | | | | | | | | | |

##### Per‑network assignment (src/chainparams.cpp)

| Network | Added types | ChainLocks | DIP0024 IS | Platform (also asset unlocks) | Mnhf (EHF) |
|---|---|---|---|---|---|
| main (272‑280) | 50_60, 60_75, 400_60, 400_85, 100_67 | 400_60 | 60_75 | 100_67 | 400_85 |
| testnet (465‑474) | same plus 25_67 | 50_60 | 60_75 | 25_67 | 50_60 |
| devnet (639‑650) | main five plus DEVNET, DEVNET_DIP0024, DEVNET_PLATFORM | DEVNET | DEVNET_DIP0024 | DEVNET_PLATFORM | DEVNET |
| regtest (913‑921) | TEST, TEST_INSTANTSEND, TEST_V17, TEST_DIP0024, TEST_PLATFORM | TEST | TEST_DIP0024 | TEST_PLATFORM | TEST |

Consensus field declarations are in src/consensus/params.h:196‑199. `llmqTypeDIP0024InstantSend`, `llmqTypePlatform` and `llmqTypeMnhf` default to `LLMQ_NONE`.

Relevant activation heights:

| Height | main | testnet | devnet | regtest |
|---|---|---|---|---|
| DIP0003Height | 1028160 | 7000 | 2 | 432 |
| DIP0008Height | 1088640 | 78800 | 2 | 1 |
| DIP0024Height | 1737792 | 769700 | 2 | 1 |
| DIP0024QuorumsHeight | 1738698 | 770730 | 2 | 1 |
| V19Height | 1899072 | 850100 | 2 | 1 |
| V20Height | 1987776 | 905100 | 2 | = DIP0003Height |
| MN_RRHeight | 2128896 | 1066900 | 2 | = V20Height |

`nInstantSendConfirmationsRequired` is 6 on main and 2 elsewhere.

##### Overrides (chainparams.cpp:1153‑1360; AddArg 1394‑1402)

Devnet only:
- `-llmqchainlocks=<name>` must be non‑rotating.
- `-llmqinstantsenddip0024=<name>` must be rotating.
- `-llmqplatform=<name>`
- `-llmqmnhf=<name>`
- `-llmqdevnetparams=<size>:<thr>` sets `size`, and sets `minSize = threshold = dkgBadVotesThreshold = thr` on LLMQ_DEVNET.

Regtest only:
- `-llmqtestparams`, `-llmqtestinstantsendparams` and `-llmqtestplatformparams` take `<size>:<thr>` and apply the same field rewrite to TEST, TEST_INSTANTSEND and TEST_PLATFORM.
- `-llmqtestinstantsenddip0024=<name>` may name a non‑rotating type. In that case the islock `cycleHash` is the quorum hash itself (see §7).
- A size above `MAX_LLMQ_SIZE` throws.

##### Which types are enabled: `ChainstateManager::IsQuorumTypeEnabled(type, pindexPrev)` (validation.cpp:6654)

- A null `pindexPrev` returns false.
- `fDIP0024IsActive = DeploymentActiveAfter(pindexPrev, DIP0024)`.
- `fHaveDIP0024Quorums = pindexPrev->nHeight >= DIP0024QuorumsHeight`.

| Type | Enabled when |
|---|---|
| DEVNET, TEST, TEST_PLATFORM, 400_60, 400_85, DEVNET_PLATFORM | always |
| 50_60 | `!fDIP0024IsActive \|\| !fHaveDIP0024Quorums \|\| testnet \|\| devnet` (retired on mainnet after DIP24 quorums) |
| TEST_INSTANTSEND | `!fDIP24 \|\| !fHaveDIP24Q \|\| llmqTypeDIP0024InstantSend == TEST_INSTANTSEND` |
| TEST_V17 | `DEPLOYMENT_TESTDUMMY` active after |
| 100_67 | `DEPLOYMENT_DIP0020` active after |
| 60_75, DEVNET_DIP0024, TEST_DIP0024 | `fDIP0024IsActive` |
| 25_67 | `pindexPrev->nHeight >= 847000` (hard‑coded testnet constant) |
| unknown | throws |

`IsQuorumRotationEnabled(params, pindex)` (options.cpp:109‑121) is true when all of these hold:
- `params.useRotation`
- `cycleBase = h - h % dkgInterval` is at least 1
- `DeploymentActiveAfter(ancestor(cycleBase - 1), DIP0024)` (note: uses the global `Params()`)

Sporks (spork.h:35‑39; all default OFF = 4070908800):

| Spork | Effect |
|---|---|
| SPORK_17_QUORUM_DKG_ENABLED (10016) | gates DKG participation |
| SPORK_21_QUORUM_ALL_CONNECTED (10020) | full mesh |
| SPORK_23_QUORUM_POSE (10022) | probes and min‑proto checks |
| SPORK_19_CHAINLOCKS_ENABLED (10018) | ChainLocks |
| SPORK_2_INSTANTSEND_ENABLED (10001) | InstantSend |

`EvalSpork` for 21 and 23 (options.cpp:85‑94): value 0 means every type; value 1 means every type except 100_67, 400_60 and 400_85; anything else means off. `IsSigningEnabled` for ChainLocks requires `SPORK_19 value == 0`, while `IsEnabled` only requires the spork to be active. IS mempool signing requires `SPORK_2 value == 0` (instantsend/signing.cpp:101‑104).

---

#### 2. Quorum member selection (src/llmq/utils.cpp)

##### Constants
- `WORK_DIFF_DEPTH = 8` (snapshot.h:40)
- `SIGN_HEIGHT_OFFSET = 8` (quorumsman.h)

These are two different constants that happen to share the value 8.

##### Score (utils.cpp:72‑85)
`score = UintToArith256(SHA256_single(dmn.confirmedHashWithProRegTxHash || modifier))`.

`confirmedHashWithProRegTxHash = SHA256_single(proTxHash_raw || confirmedHash_raw)` (evo/dmnstate.h:162‑169). Both hashes are single SHA256, not double. The arith interpretation reads the 32 bytes as a little‑endian 256‑bit integer.

##### CalculateQuorum (utils.cpp:160‑186)
1. Candidates:
   - From a `CDeterministicMNList`: `ForEachMNShared(onlyValid=true)`, so PoSe‑banned MNs are excluded.
   - From a vector (rotation "not used" list): skip `IsBanned()`.
   - In both cases skip `confirmedHash.IsNull()`, and when `onlyEvoNodes` is set, skip non‑Evo MNs.
2. Sort `std::sort(rbegin, rend, cmp)` where `cmp(a,b) = (a.score == b.score) ? a.collateral < b.collateral : a.score < b.score`. The result in forward order is score descending; on ties it is **collateral outpoint descending**.
3. Truncate to `maxSize` if `maxSize > 0`.

##### Modifier (utils.cpp:87‑115)
`pWorkBlock = cycleBase.GetAncestor(cycleBase.h - 8)`.

- If `DeploymentActiveAfter(pWorkBlock, V20)`:
  - If the work block's coinbase has a non‑null CL (`GetNonNullCoinbaseChainlock`, evo/cbtx.cpp:181‑211: CbTx version ≥ CLSIG_AND_BALANCE, nHeight matches, sig `IsValid`, `bestCLHeightDiff < height`): `modifier = H(tuple(llmqType u8, workBlock.nHeight int32, bestCLSignature 96B))`. The signature is serialized with the global BLS scheme flag; post‑V19 that means basic encoding.
  - Otherwise: `H(pair(llmqType, workBlockHash))`.
- Else (pre‑V20):
  - Rotation: `H(pair(llmqType, workBlockHash))`.
  - Non‑rotation: `H(pair(llmqType, cycleBaseBlockHash))`.

##### Non‑rotated members (`GetAllQuorumMembers`, utils.cpp:634‑730; `ComputeQuorumMembers` 291‑305)
- Returns `{}` if `base.pprev == null` or the type is not enabled at `base.pprev`.
- MN list block: `base-8` if `DeploymentActiveAfter(base, V20)`, otherwise `base` itself. The modifier rule above checks V20 at `base-8`, so the two conditions use different V20 checks at the boundary.
- Requires `base.h % dkgInterval == 0`.
- `EvoOnly = (type == llmqTypePlatform) && DeploymentActiveAfter(base, V19)`.
- Result is `CalculateQuorum(list, modifier, size, EvoOnly)`. It can have fewer than `size` members.

##### Rotated members (DIP0024, utils.cpp:188‑540)
- `quorumIndex = base.h % dkgInterval`. It must be below `signingActiveQuorumCount`, otherwise the result is empty.
- `cycleBase = base.h - quorumIndex`. The work block is `cycleBase - 8`. The MN list is taken at the work block.
- **Previous quarters:** for k = 1, 2, 3, take `cycleBase - k*C` (C = dkgInterval), load its snapshot (`llmq_S`) and rebuild its quarters with `GetQuorumQuarterMembersBySnapshot` (188‑289):
  1. `sortedAllMns = CalculateQuorum(list@thatWorkBlock, modifier(thatCycle))`.
  2. Walk it: entries with `snapshot.activeQuorumMembers[i]` go to `usedMNs`; others go to the front list unless banned. `combined = notUsed ++ used`.
  3. `quarterSize = size/4`.
  4. `MODE_NO_SKIPPING (0)`: fill each of `signingActiveQuorumCount` quarters sequentially, wrapping around.
  5. `MODE_SKIPPING_ENTRIES (1)`: decode the skip list. The first element is absolute; later elements are offsets from the first. A quirk: while `first_entry_index` is still 0, the next entry is treated as absolute too. Fill sequentially, skipping listed indices and wrapping.
  6. Modes 2 and 3: return empty quarters.
  7. If a snapshot is missing or a cycle index is null, stop at that cycle (or return empty when predicting).
- **New quarter (`BuildNewQuorumQuarterMembers`, 343‑466):**
  1. If `allMns.enabled < quarterSize`, return empty quarters **and store no snapshot**.
  2. `skipRemovedMNs = cycleBase + 1 >= V19Height || testnet`.
  3. Build `MnsUsedAtH` (and `MnsUsedAtHIndexed[i]`) from all previous quarters. Skip MNs that were removed (when `skipRemovedMNs`) or are PoSe‑banned. `AddMN` exceptions (uniqueness collisions) are swallowed.
  4. `combined = CalculateQuorum(notUsed, mod) ++ CalculateQuorum(MnsUsedAtH, mod)`.
  5. For each index i: walk `combined` from a shared cursor `idx`, taking entries not already in `MnsUsedAtHIndexed[i]`. Each skipped index is recorded in the skip list (first absolute, later relative to the first). The loop ends when `quarter.size == quarterSize` or `usedCount + quarter.size >= combined.size`. A full lap with no addition returns all‑empty quarters.
  6. When not predicting, store the snapshot for the cycle base: `activeQuorumMembers` has length `allMns.total()` (including banned) and marks positions in `CalculateQuorum(allMns)` that are in `MnsUsedAtH`. The mode is 0 for an empty skip list, else 1.
- **Final member list for index i** = `quarter(H-3C)[i] ++ quarter(H-2C)[i] ++ quarter(H-C)[i] ++ new[i]`.
- Members are cached per (cycleBaseHash, i) and per baseHash.
- `PreComputeQuorumMembers` (blockprocessor.cpp:35‑43) runs in `ProcessBlock` at every cycle‑base block. It is therefore what creates the snapshot during block connection, and it resets the cache on undo.
- `ComputeQuorumMembersFromWorkBlock` (589+) predicts a future quorum from its work block. It only works after V20.

##### Connections (not consensus)
- `DeterministicOutboundConnection(a, b)` (utils.cpp:732): `min = min(a,b)`, `max = max(a,b)`. Compare `H(tuple(min, max, a))` with `H(tuple(min, max, b))`; the lower hash initiates.
- Relay ring (`GetQuorumRelayMembers`, 778): from member i, connect to `(i + 2^k) % n` while `(gap_max >>= 1) || k <= 1`, with `gap_max = n - 1`.
- Watchers (`CalcDeterministicWatchConnections`, 833): `rnd = H(pair(rnd, pair(type, baseHash)))` with a random per‑process seed.
- With all‑connected enabled, it is a full mesh using the deterministic initiator rule.

---

#### 3. DKG

##### Phase clock (active/dkgsessionhandler.cpp:43‑70)
- `stage = (tipHeight - quorumIndex) % dkgInterval`.
- On `stage % dkgPhaseBlocks == 0`, `phase = stage / dkgPhaseBlocks + 1`, clamped to 1..7.
- `QuorumPhase` values (dkgsessionhandler.h): Initialized = 1, Contribute = 2, Complain = 3, Justify = 4, Commit = 5, Finalize = 6, Idle = 7.

With `phaseBlocks = 2`:

| Stage | Phase |
|---|---|
| 0 | Init |
| 2 | Contribute |
| 4 | Complain |
| 6 | Justify |
| 8 | Commit |
| 10 | Finalize |
| 12+ | Idle |

The mining window starts at `5 * phaseBlocks`, measured from the cycle start. For rotated types, quorum i's DKG is shifted by i blocks, so the window starts at `activeCount + 10` (42 for 60_75). Handlers exist per (type, i) for `i < signingActiveQuorumCount` when rotating, otherwise once (dkgsessionmgr.h:94‑104). Indexes above 0 only run when rotation is enabled.

##### Round driver (`NetDKG::HandleDKGRound`, net_dkg.cpp:833‑936)
1. Wait for Initialized.
2. Clear the queues.
3. `InitNewQuorum`:
   - DIP3 must be enforced and the node must not have an unvalidated snapshot chainstate.
   - `CDKGSession::Init` aborts if `members < minSize`.
4. Single‑member quorums skip straight to `FinalizeSingleCommitment`.
5. `EnsureQuorumConnections`, plus probe connections if SPORK_23 is on.
6. Each phase calls `HandlePhase(cur, next, startFunc, waitFunc)`:
   - Sleep `myIdx * ((phaseBlocks - 1) * nPowTargetSpacing * 1000 / size) * factor` ms. Factor is 0.05, or 0.1 for Commit. There is no sleep on regtest or for non‑members.
   - Run the start function.
   - Batch‑process pending messages, 8 at a time, until the next phase.
7. After Commit, call `FinalizeCommitments` and send each result to `AddMineableCommitment`, which relays an inv.

##### Messages (dkgmessages.h)

All four messages begin with `llmqType u8 | quorumHash | proTxHash`.

| cmd | inv type | Remaining fields | Sign hash |
|---|---|---|---|
| `qcontrib` | MSG_QUORUM_CONTRIB = 23 | `vvec: vector<CBLSPublicKey>` (length = threshold), `contributions: CBLSIESMultiRecipientObjects<CBLSSecretKey>` (`ephemeralPubKey 48 \| ivSeed 32 \| vector<vector<u8>> blobs`), `sig 96` | `H(all fields except sig, then 96 zero bytes)` |
| `qcomplaint` | MSG_QUORUM_COMPLAINT = 24 | `DYNBITSET badMembers`, `DYNBITSET complainForMembers`, `sig` | `H(msg with sig = null)` |
| `qjustify` | MSG_QUORUM_JUSTIFICATION = 25 | `vector<{uint32 index, CBLSSecretKey 32}>`, `sig` | `H(msg with sig = null)` |
| `qpcommit` | MSG_QUORUM_PREMATURE_COMMITMENT = 26 | `DYNBITSET validMembers`, `quorumPublicKey 48`, `quorumVvecHash`, `quorumSig 96`, `sig 96` | `BuildCommitmentHash(type, quorumHash, validMembers, quorumPublicKey, vvecHash)` |
| `qwatch` | – | empty; sets `pfrom.qwatch` (MN only) | – |

`sig` is the member's **operator key** signature. It is created with `legacy = !DeploymentActiveAfter(base, V19)` and verified with the global scheme flag.

`DYNBITSET` is a CompactSize bit count followed by `ceil(n/8)` bytes, LSB‑first. When reading, bits set beyond n in the last byte cause a throw ("Out-of-range bits set", serialize.h:452‑471).

##### Intake (net_dkg.cpp:492‑666)
A DKG message is rejected or penalized when:
- The sender is not MNAuth‑verified (+10).
- The llmqType is unknown (+100).
- The base block is unknown (+10).
- The base block is parentless (+100).
- The type is not enabled (+100).
- `quorumIndex = h % C` exceeds the maximum (+100).
- The size exceeds `MaxDKGMessageSize` (net_dkg.cpp:43‑75) (+100).
- The wire framing walk fails (vvec count == threshold, `minSize <= blobs <= size`, equal bitset sizes ≤ size, and so on) (+100).
- It was not requested via GETDATA (`UNREQUESTED_OBJECT_MISBEHAVIOR_SCORE`).

Accepted messages are queued per proTx with a quota of `size * 2`. Messages are relayed only via inv/getdata, to `relayMembers`.

##### Session logic
1. **Contribute** (active/dkgsession.cpp:40‑121):
   - `GenerateContributions(threshold, memberIds)` creates `threshold` random secret keys (polynomial coefficients). `vvec[j] = sk_j·G1`.
   - `skShare_m = Poly(sk)(id_m)`, where `id_m = CBLSId(proTxHash)`.
   - Encrypt each share to that member's `pubKeyOperator`.
2. **Receive contribution** (dkgsession.cpp:207‑260):
   - PreVerify: right quorum, sender is a member (else ban), sig valid, `blobs == members.size()`, `vvec.size == threshold`, vvec entries valid and unique, fewer than 2 prior contributions.
   - A second message from the same member marks it bad, but the message is still relayed.
   - Members persist the vvec (`qdkg_V`), decrypt their own blob, and queue share verification (batched in groups of 32).
   - `VerifyContributionShares` checks `sk.GetPublicKey() == PublicKeyShare(vvec, myId)` and writes `qdkg_S`.
   - Encrypted contributions go to `qdkg_E`.
   - A decrypt failure or a bad share sets `weComplain`.
3. **Complain** (active/dkgsession.cpp:186‑332):
   - Members that sent no contribution are marked bad.
   - With PoSe on, `badConnection` is set for a member that has no MN connection, has `version < MIN_MASTERNODE_PROTO_VERSION (70242)`, does not relay txs, does not serve compact filters, failed outbound too many times, or is Platform‑banned.
   - The complaint sets `badMembers[i]` for bad or badConnection members and `complainForMembers[i]` for `weComplain`. Nothing is sent if both sets are empty.
4. **Justify** (334‑421):
   - A member with `badMemberVotes >= dkgBadVotesThreshold` becomes bad.
   - A member with more than one complaint becomes bad.
   - For every complaint that names us, reveal the plaintext share in a justification.
   - On receipt (dkgsession.cpp:422‑495): a justification for someone who did not complain marks the justifier bad. Each revealed share is verified against the justifier's vvec; an invalid one marks the justifier bad. A valid one addressed to us replaces our share and clears `weComplain`.
5. **Commit** (423‑566):
   - Members that still have open `complaintsFromOthers` become bad.
   - `validMembers = !bad`. Abort if the count is below `minSize`.
   - Using verified contributions for the valid set, `quorumVvec[j] = Σ vvec_m[j]` and `skShare = Σ sk_m`.
   - `quorumPublicKey = quorumVvec[0]`, `quorumVvecHash = H(vector<CBLSPublicKey>)` (scheme flag dependent).
   - `qc.sig = operatorSign(commitmentHash)`, `qc.quorumSig = skShare.Sign(commitmentHash)`.
   - On receipt (dkgsession.cpp:540‑602), a node that can rebuild the vvec checks pubkey, vvecHash and the share signature. Invalid messages are not relayed.
6. **Finalize** (581‑697):
   - Group valid premature commitments by identical `validMembers`. Ignore groups with fewer than `minSize`.
   - Keep entries matching the first entry's pubkey and vvecHash. Set `signers[idx]`.
   - `membersSig = AggregateSecure(sigs, operatorPubKeys, commitmentHash)`.
   - `quorumSig = Recover(Lagrange over member ids)`.
   - `nVersion = GetVersion(rotationEnabled(base), V19ActiveAfter(base))`, `quorumIndex = rotation ? i : 0`.
   - Verify fully, then add to the mineable pool.
7. **Single‑member quorum** (699‑745): `quorumPublicKey = operator pubkey`, `quorumSig = membersSig = operatorSign(commitmentHash)`, vvecHash is null.

Batched DKG signature check (`BatchVerifyMessageSigs`, net_dkg.cpp:217‑321): aggregate `AggregateInsecure`, then `VerifyInsecureAggregated(pubkeys, signHashes)`. If any signHash is duplicated, fall back to per‑message verification. If all messages came from one node, blame that node.

##### `CFinalCommitment` (commitment.h:45‑145)

Versions:

| nVersion | Name |
|---|---|
| 1 | LEGACY_BLS_NON_INDEXED |
| 2 | LEGACY_BLS_INDEXED |
| 3 | BASIC_BLS_NON_INDEXED |
| 4 | BASIC_BLS_INDEXED |

Serialization, in order:
1. `nVersion u16`
2. `llmqType u8`
3. `quorumHash`
4. `quorumIndex int16`, present only if v ∈ {2, 4}
5. `DYNBITSET signers`
6. `DYNBITSET validMembers`
7. `quorumPublicKey 48`
8. `quorumVvecHash 32`
9. `quorumSig 96`
10. `membersSig 96`

The three BLS fields are encoded legacy iff v ∈ {1, 2}, using explicit version wrappers that do not depend on the global flag.

`IsNull` means no set bits, pubkey invalid, vvecHash null, and both sigs invalid.

`BuildCommitmentHash` (commitment.cpp:312‑323) = `CHashWriter << llmqType << blockHash << DYNBITSET(validMembers) << pubKey << vvecHash`. **`pubKey` here uses the global scheme flag.**

`Verify(util_params{base}, checkSigs)` (113‑209) requires:
- The type exists and `base.pprev` is non‑null.
- `nVersion == GetVersion(IsQuorumRotationEnabled(params, base), DeploymentActiveAfter(base, V19))`.
- `base.hash == quorumHash`.
- `base.h % dkgInterval == quorumIndex`, which also applies to non‑indexed versions, where the index is 0.
- Both bitset sizes equal `params.size`.
- `validCount >= minSize` and `signerCount >= minSize`.
- pubKey valid, vvecHash non‑null (unless single member), and both sigs valid.
- No bits set at positions `>= members.size()`.
- When `checkSigs` is set: `membersSig.VerifySecureAggregated(signerOperatorPubKeys, commitmentHash)` (single member: `VerifyInsecure` with members[0]'s operator key) and `quorumSig.VerifyInsecure(quorumPublicKey, commitmentHash)`.

`VerifyNull` only requires `IsNull()` and correct bitset sizes. **The null commitment's version is not checked.**

##### Mining rules (blockprocessor.cpp)

The qc special transaction is type 6, tx nVersion 3, with no inputs and no outputs (tx_check.cpp:18). It is rejected from the mempool with `qc-not-allowed` (validation.cpp:855). Its payload `CFinalCommitmentTxPayload` is `{u16 nVersion = 1, u32 nHeight, CFinalCommitment}`.

`CheckLLMQCommitment` (commitment.cpp:240‑310) rejects with:
- `bad-qc-payload` when the payload does not decode.
- `bad-qc-commitment-type` for an unknown type.
- `bad-qc-version` unless the payload nVersion is 1.
- `bad-qc-height` unless `nHeight == block height`.
- `bad-qc-quorum-hash` when quorumHash is unknown or not an ancestor.
- Null commitments then only go through `VerifyNull` (`bad-qc-invalid-null`).
- Otherwise `Verify(checkSigs=false)` (`bad-qc-invalid`).

`ProcessBlock` (212‑308) does the following, in order:
1. Before DIP3 activation, just writes `q_bbu2` and returns.
2. `PreComputeQuorumMembers`.
3. `GetCommitmentsFromBlock` (564‑604):
   - For a non‑rotation type, a second commitment of that type in the block (null or not) gives `bad-qc-dup`.
   - Any commitment before DIP3 gives `bad-qc-premature`.
4. For each type enabled at `pindex.pprev`, unless the chainstate tip is null (crash replay):
   - `required = GetNumCommitmentsRequired` (606‑624): 0 outside the mining window. Otherwise, for each quorumIndex in `0..(rotation ? activeCount : 1)` where the base block exists and has no mined commitment in this chain, count one.
   - The count in the block must equal `required` exactly. Too many gives `bad-qc-not-allowed`; too few gives `bad-qc-missing`.
5. Any non‑null `(type, quorumHash)` duplicated within the block gives `bad-qc-dup`. Null commitments are exempt.
6. If `fBLSChecks`, verify all non‑null commitment signatures in parallel (`bad-qc-invalid` on failure).
7. `ProcessCommitment` (326‑446) for each commitment:
   - `quorumHash` must equal `chain[h - h % C + quorumIndex]` (`bad-qc-block`).
   - Null commitments are checked with `VerifyNull` only.
   - A stored different commitment for the same quorum gives `bad-qc-dup`.
   - The height must be in `[cycle + mineStart, cycle + mineEnd]` (`bad-qc-height`).
   - `Verify(no sigs)`.
   - Write `q_mc` and the inversed‑height key.
8. Write `q_bbu2`.

The mining window is `IsMiningPhase` (311‑324). `UndoBlock` (519‑562) erases the keys (unless another chainstate still references the block) and puts the commitment back into the mineable pool.

The miner (`GetMineableCommitments`, 882‑935) builds a null commitment `CFinalCommitment(params, quorumHash)` with `quorumIndex` and the proper version when nothing better is known.

`qfcommit` P2P (98‑210) is accepted only when solicited. It is rejected if null (+100), if the type is invalid (+100), if `quorumHeight != base.h` (+100), or if `base.h < tip - dkgInterval` (+100 for peers at version 70239 or later). It is ignored if already mined or if we already hold a commitment with at least as many signers. Otherwise `Verify(sigs)` runs and the commitment is added to the pool, replacing any entry with fewer signers.

Also outside the in‑scope directories: `HandleQuorumCommitment` (evo/specialtxman.cpp:284‑299) applies a PoSe penalty of `CalcPenalty(66)` to every member with `!validMembers[i]`. This changes the MN list, so it is consensus.

##### DKG persistence: `CDKGSessionManager` (dkgsessionmgr.cpp:24‑35)

The leveldb is `<datadir>/llmq/dkgdb`.

| Key | Value |
|---|---|
| `("qdkg_V", type, baseHash, proTxHash)` | CompactSize + vvec, **always basic encoding** |
| `("qdkg_S", …)` | CBLSSecretKey (32 bytes) |
| `("qdkg_E", …)` | `CBLSIESMultiRecipientObjects` |

`CleanupOldContributions` erases entries deeper than `max_store_depth`. The contributions cache lives for 60 s.

`quorum dkgstatus` is local only, from `CDKGDebugManager` (debug.h). Member status bits: bad, weComplain, received{Contribution, Complaint, Justification, PrematureCommitment}. Session bits: sent{…}, aborted. The old P2P message `MSG_QUORUM_DEBUG_STATUS = 27` has been removed.

---

#### 4. Quorum manager (quorumsman.cpp, quorums.cpp, net_quorum.cpp)

`CQuorum` holds:
- `params`, `qc`, `m_quorum_base_block_index`, `minedBlockHash`
- `members` (all, including those that failed the DKG)
- optionally `quorumVvec` and `skShare`

`GetPubKeyShare(i)` returns `PublicKeyShare(vvec, CBLSId(member.proTxHash))`, and only for valid members. `SetVerificationVector` requires `H(vvec) == qc.quorumVvecHash`. `SetSecretKeyShare` requires `sk·G == GetPubKeyShare(myIdx)`.

##### ScanQuorums(type, pindexStart, n) (quorumsman.cpp:170‑286) — consensus relevant
The scan start is snapped to the DKG mining window:
- If `h < cycle + mineStart`, use `ancestor(cycle + mineEnd - C)`. If that would be below the genesis cycle, return `{}`.
- If `h > cycle + mineEnd`, use `ancestor(cycle + mineEnd)`.
- Otherwise use the block as is.

Then:
- Non‑rotation: walk the `q_mcih` index (mined height ≤ start) and take the newest n.
- Rotation: `GetMinedCommitmentsIndexedUntilBlock` takes, per cycle back, the latest mined commitment per quorumIndex in index order 0..activeCount‑1.

Results are newest first. A missing quorum causes `{}`. Types not enabled at `pindexStart` give `{}`. `ScanCommitments` follows the same rules but returns raw commitments.

##### Keys and storage
evodb, written in the block transaction:

| Key | Value |
|---|---|
| `("q_mc", (type, quorumHash))` | `pair(CFinalCommitment, minedBlockHash)` |
| `("q_mcih", type, be32(0xFFFFFFFF - minedHeight))` | `int quorumHeight` |
| `("q_mcihi", type, int32LE quorumIndex, be32(0xFFFFFFFF - minedHeight))` | `int quorumHeight` (rotation) |
| `"q_bbu2"` | best block hash |

`<datadir>/llmq/quorumdb` holds `("q_Qqvvec", quorumKey)` (CompactSize + basic‑encoded vvec) and `("q_Qsk", quorumKey)` (32‑byte share). `quorumKey = CHashWriter(type, quorumHash, each member proTxHash)`. These entries were migrated from evodb by `MigrateOldQuorumDB` (649‑709) and are cleaned every 576 blocks at `h % 576 == 58`, keeping the newest `keepOldKeys` per type.

##### Building a quorum
`BuildQuorumFromCommitment`: read `q_mc`, compute members, then try to read the vvec and sk from quorumdb. If that fails, build them from verified DKG contributions. Size‑1 quorums skip this step. Built quorums are warmed in the background by precomputing every pubkey share.

##### Data recovery

`-llmq-data-recovery` (default 1) and `-llmq-qvvec-sync=<name>:<0|1>` (0 = always, 1 = only if member of that type) control it.

`qgetdata` request: `llmqType u8 | quorumHash | u16 dataMask | proTxHash`. `dataMask` bit 1 = vvec, bit 2 = encrypted contributions.

`qdata` response: the same header plus `u8 nError`, then the vvec (`vector<CBLSPublicKey>` of size threshold) and/or `vector<CBLSIESEncryptedObject<SK>>` (one per valid member, each `{ephPK, ivSeed, blob}` and decrypted at `memberIdx`). Error codes run 0..6, with 0xFF = UNDEFINED.

Rules:
- Only MNs answer, and only to MNAuth‑verified peers or qwatch peers.
- Requests expire after 300 s (+60 s bias).
- At most 64 requests per requester and 4096 in total.
- The receiver reconstructs `skShare = Σ decrypted` and checks it against the pubkey share.
- The recovery start offset is the index of our proTx in the sorted valid‑MN list, modulo `validMembers.size()`.

##### Connections and watching
`CheckQuorumConnections` runs for the newest `keepOldConnections` quorums. Members connect to their outbound set; watchers (`-watchquorums`, debug only, default 0) connect to one random member. IS‑type members also watch other IS quorums. The connections for the in‑progress DKG are kept.

---

#### 5. Signing

##### Sign hash (signhash.cpp)
`SignHash = CHashWriter(type u8, quorumHash, id, msgHash).GetHash()`, that is, **double SHA256** over 97 bytes.

##### CRecoveredSig (signing.h)
Fields: `type u8 | quorumHash | id | msgHash | sig 96`. The sig is a `CBLSLazySignature`, whose encoding scheme is taken from the global flag at construction or decode. The inv hash is `H(serialized)` with inv type `MSG_QUORUM_RECOVERED_SIG = 28`.

##### Quorum selection (quorumsman.cpp:711‑782)
- Start block: `SelectQuorumForSigningStartBlock(chain, signHeight = tip, offset = 8)` returns `chain[signHeight - offset]`, or null if out of range.
- Non‑rotated: take `ScanQuorums(type, start, activeCount)`. Pick the quorum with the minimum `CHashWriter(type, quorumHash, selectionHash)`. The comparison is `uint256 operator<`, a **memcmp over raw little‑endian bytes**, not a numeric compare.
- Rotated: take `ScanQuorums(type, start, activeCount)`. Compute `n = log2(activeCount)` (5 for 60_75), `b = selectionHash.GetUint64(3)` (bytes 24..31, little‑endian), and `signer = (b >> (64 - n - 1)) & ((1 << n) - 1)`. This uses bits 58..62, not the top bit. Return the quorum with `qc.quorumIndex == signer`; if none matches (or `signer > quorums.size()`), return null.
- `VerifyRecoveredSig(type, start, id, msgHash, sig)`: select a quorum with `selectionHash = id`, then `sig.VerifyInsecure(quorumPublicKey, SignHash(type, quorum.quorumHash, id, msgHash))`. The result is NoQuorum, Invalid or Valid.

`IsQuorumActive(type, hash)` means the quorum is among `ScanQuorums(tip, keepOldConnections)`. Recovered sigs and shares from inactive quorums are dropped.

##### Recovered sig DB (signing.cpp:19‑370)
The leveldb is `<datadir>/llmq/recsigdb` (8 MiB cache).

Non‑Platform types:
- `("rs_r", type, id)` → CRecoveredSig
- `("rs_r", type, id, msgHash)` → u32 write time
- `("rs_t", be32 time, type, id)` → 1

Platform type (several messages per id are allowed):
- `("rs_p", hash)` → recSig
- `("rs_m", type, id, msgHash)` → hash
- `("rs_u", be32 time, hash)` → 1

All types:
- `("rs_h", hash)` → `(type, id)`
- `("rs_s", signHash)` → 1
- Votes: `("rs_v", type, id)` → msgHash and `("rs_vt", be32 time, type, id)` → 1

Expiry is `-maxrecsigsage`, default 604800 s. `TruncateRecoveredSig` keeps the `rs_h` and `rs_s` keys so that AlreadyHave still returns true.

A conflict means a different msgHash for the same (type, id) on a non‑Platform type. `ProcessRecoveredSig` refuses to write a second sig for the same id.

##### Signature shares (signing_shares.{h,cpp}; net_signing.cpp)

| cmd | Payload |
|---|---|
| `qsigsesann` | `vector ≤100 of {VARINT sessionId, type, quorumHash, id, msgHash}` |
| `qsigsinv` / `qgetsigs` | `vector ≤200 of {VARINT sessionId, CompactSize n ≤ 400, AUTOBITSET}` |
| `qbsigs` | `vector of {VARINT sessionId, LIMITED_VECTOR ≤400 of (u16 member, sig96)}`; total shares ≤ 400 |
| `qsigshare` | (SPORK_21 only) `vector ≤32 of CSigShare{type, quorumHash, u16 quorumMember, id, msgHash, sig}` |
| `qsigrec` | CRecoveredSig |
| `qsendrecsigs` | bool |

`AUTOBITSET` is one flag byte. 0 means a fixed bitset follows; 1 means VARINT gap encoding follows. The gap encoding is `first = idx + 1`, then deltas, terminated by 0. The writer picks whichever is smaller.

Signing (`AsyncSignIfMember`, 1009+):
1. Pick the quorum: `SelectQuorumForSigning(tip - 8)`, or an explicit quorumHash.
2. Refuse if the node voted (`rs_v`) for a different msgHash for this id, unless `allowDiffMsgHashSigning` (Platform, EHF).
3. Skip if a recovered sig already exists.
4. Otherwise record the vote.
5. `share = skShare.Sign(signHash)` with the global scheme flag. A size‑1 quorum signs with the operator key instead.

Recovery (`TryRecoverSig`): once `threshold` shares are present, `Recover(sigs, ids = member proTxHash)` by Lagrange interpolation.

Verification:
- Incoming shares are batch‑verified against `GetPubKeyShare(member)`.
- Recovered sigs are batch‑verified against `quorumPublicKey`. Pending caps: 1000 per node, 10000 in total.

With all‑connected enabled, each member sends its own share directly to recovery members. The recovery member order is `sort by H(pair(proTxHash, id))`. A member is tried at attempt k, with backoff `min(2^sendCount * 2 s, 10 s)` and `recoveryMembers` attempts in total.

---

#### 6. ChainLocks

Wire format: `clsig` = `ChainLockSig{int32 nHeight, uint256 blockHash, CBLSSignature sig}`, inv `MSG_CLSIG = 29`, hash `H(clsig)`.

- Request id: `GenSigRequestId(h) = H(pair(string_view "clsig", int32 h))`. That is `0x05 "clsig" || int32LE`, double‑SHA256 (clsig.cpp:226‑229).
- Message hash: the block hash.
- LLMQ type: `llmqTypeChainLocks`.
- Verification (`VerifyChainLock`): `VerifyRecoveredSig(type, chain, qman, signedAtHeight = clsig.height, id, blockHash, sig)`. The quorum is chosen from the quorum set at `chain[height - 8]`.

Signing (chainlock/signing.cpp:73‑163) happens when all of these hold:
- The node is synced.
- SPORK_19 is active with value 0.
- The tip has not already been signed.
- The best CL is below the tip height and does not conflict with the tip.
- When IS is enabled, every non‑coinbase tx in the tip and the previous 5 blocks (`TX_CONFIRM_THRESHOLD = 5`), stopping at a ChainLocked block, is either islocked or was first seen at least 10 min ago.

The signer calls `AsyncSignIfMember(type, id, tipHash)`. On recovery it builds a CLSIG and processes it locally.

Processing (handler.cpp:116‑160):
- Duplicates are suppressed via the seen cache.
- Heights at or below the best CL are ignored.
- An invalid signature from a peer gives +10.
- `UpdateBestChainlock` is called. If the block is known, enforcement is scheduled.

Enforcement: `EnforceBlock`, then `ActivateBestChain`.

Validation effects:
- `HasConflictingChainLock(h, hash)` (true when best‑CL ancestry at h differs) rejects:
  - headers (`bad-chainlock`, status `BLOCK_CONFLICT_CHAINLOCK`, validation.cpp:4691),
  - ConnectBlock (validation.cpp:2517),
  - TestBlockValidity (validation.cpp:4935).
- A ChainLocked block overrides conflicting islocks; otherwise a conflicting block is rejected with `conflict-tx-lock` (validation.cpp:2802‑2819).

CbTx fields (V20+, `CLSIG_AND_BALANCE`): `bestCLHeightDiff` (CompactSize) and `bestCLSignature`. The certified height is `h - diff - 1`. `CheckCbTxBestChainlock` (evo/specialtxman.cpp:139‑214) rejects with:
- `bad-cbtx-null-clsig` if the previous coinbase had a CL and this one does not.
- `bad-cbtx-older-clsig` if `diff > prevDiff + 1`.
- `bad-cbtx-cldiff` if `diff >= h`.
- `bad-cbtx-cldiff` if the CL is null but `diff != 0`.
- `bad-cbtx-invalid-clsig` if `VerifyChainLock` fails.

The miner (node/miner.cpp:116‑170) excludes CLs below V19Height.

Persistence: there is none. The best CL is held in memory. On startup it is rebuilt from coinbase CLs (`ProcessCoinbaseChainLock`) and from peers. Options: `-chainlocknotify`.

---

#### 7. InstantSend (DIP0022 deterministic islock only)

`isdlock` wire format, inv `MSG_ISDLOCK = 31` (`MSG_ISLOCK = 30` is retired):
1. `u8 nVersion = 1`
2. `vector<COutPoint> inputs`
3. `txid`
4. `cycleHash`
5. `sig 96` (lazy)

The hash is `H(islock)`. `TriviallyValid` requires: non‑null txid, inputs non‑empty, inputs ≤ `MaxBlockSize() / 41`, inputs unique.

Request ids:
- Per‑input lock ("inlock"): `GenInputLockRequestId(op) = H(pair("inlock", COutPoint))`, with msgHash = txid.
- islock: `GetRequestId() = CHashWriter("islock", inputs vector)`, with msgHash = txid.
- Asset unlock transactions are lockable through the synthetic outpoint `{H(pair("plwdtx", u64 index)), 0}` (lock.cpp:39‑50). Only stable‑txid (v2) unlocks are signed.

Signing flow (instantsend/signing.cpp:330‑474):
1. `CheckCanLock`: every parent is islocked, or mined with `age >= nInstantSendConfirmationsRequired`, or ChainLocked. A parent still in the mempool and not locked fails.
2. Abort if there is a conflicting islock.
3. Sign each input id with the IS type (quorum via `SelectQuorumForSigning(tip - 8, id)`).
4. When every input has a recovered sig, build the islock. The quorum is `SelectQuorumForSigning(islock id)` and `cycleHash = ancestor(quorumBase.h - quorumBase.h % C).hash`. Sign with that explicit quorum.
5. Retroactive signing happens on BlockConnected for unlocked txs, even if mempool signing is disabled (spork 2 ≠ 0).

Verification (net_instantsend.cpp:104‑360):
1. The cycleHash height must be known and satisfy `% C == 0`, otherwise +misbehavior.
2. `nSignHeight = (cycleH + C < tipH) ? cycleH + C - 1 : tip`.
3. First pass: quorum = `SelectQuorumForSigning(params, chain, id, nSignHeight, signOffset = 0)`, batch verify, no ban.
4. Failures are retried with `signOffset = dkgInterval`, and banned if that also fails.
5. For a non‑rotating IS type (regtest override), `cycleHash` is the quorum hash, used directly.
6. A verified islock pushes a reconstructed CRecoveredSig.

Effects:
- Rejection: an islock whose inputs do not equal `GetLockInputs(tx)` is rejected.
- ChainLock interaction: an islock for a tx already in a ChainLocked block is dropped. If a conflicting tx was mined in a ChainLocked block, the islock (and its descendants) is removed. Otherwise conflicting blocks are invalidated and ActivateBestChain is called (`ResolveBlockConflicts`). The input recsigs are truncated.
- Mempool: `tx-txlock-conflict` (validation.cpp:891‑899). Conflicts are removed when a lock arrives.
- Gates: `IsInstantSendEnabled = !reindex && !importing && SPORK_2 active`. SPORK_3 no longer exists. `-instantsendnotify` is a wallet option that replaces `%s` with the txid and `%w` with the wallet name.

DB: `<datadir>/llmq/isdb` (32 MiB cache), version key `is_v = 1`. A missing or old version wipes the DB.

| Key | Value |
|---|---|
| `("is_i", hash)` | islock |
| `("is_tx", txid)` | hash |
| `("is_in", outpoint)` | hash |
| `("is_m", be32(~h), hash)` | mined |
| `("is_a1", be32(~h), hash)` | archived (by height) |
| `("is_a2", hash)` | archived (by hash) |

On a ChainLock, confirmed locks are removed. Archived locks are pruned once older than 100 blocks.

---

#### 8. BLS

##### Library and schemes
The library is the vendored dashbls 2.1 (relic 0.5.0, modified). Curve BLS12‑381, minimal‑pubkey: G1 pubkeys are 48 bytes, G2 signatures are 96 bytes, secret keys are 32‑byte big‑endian scalars. Ids are 32 bytes.

Schemes (bls.cpp:20‑27):
- **Legacy** = `LegacySchemeMPL`. Hash‑to‑G2 is `ep2_map_legacy` (dashbls/src/legacy.cpp:306+):
  - The message must be exactly 32 bytes.
  - Four field elements are derived. Each is `SHA256(msg || "G2_a_cb" || 0x00) || SHA256(msg || "G2_a_cb" || 0x01)`, 64 bytes read big‑endian and reduced mod p, for a ∈ {0,1} and b ∈ {0,1}.
  - Apply Fouque‑Tibouchi/SW encoding (zkcrypto style) to each Fq2 pair, add the two points, then clear the cofactor with the BLS12 ψ method.
  - There is no DST.
- **Basic** = `BasicSchemeMPL`, IETF hash_to_curve with DST `"BLS_SIG_BLS12381G2_XMD:SHA-256_SSWU_RO_NUL_"` (schemes.cpp:81). Basic `AggregateVerify` requires distinct messages.
- Threshold operations always use legacy‑scheme Lagrange; the scheme makes no difference to them.

##### Global scheme flag
`bls::bls_legacy_scheme` starts true. For each block, `ConnectBlock` sets it to `!DeploymentActiveAt(block, V19)` (validation.cpp:2491). After the block before activation is processed, `ProcessSpecialTxsInBlock` flips it to false (specialtxman.cpp:986‑991). Undo flips it back. The flag controls default (de)serialization, verification and aggregation.

##### Encodings (dashbls/src/elements.cpp)
Relic in dashbls was modified so that the sign bit means "y > (p‑1)/2". For G2, only y.c1 is compared (relic_ep2_pck.c).

| Element | Legacy | Basic |
|---|---|---|
| G1 | 48‑byte x, big‑endian; bit 0x80 of byte 0 = y sign; no compression flag | IETF/ZCash: 0x80 compression \| 0x40 infinity \| 0x20 sign |
| G2 | relic order x.c0 \|\| x.c1; 0x80 on byte 0 = sign | x.c1 \|\| x.c0; flags in byte 0 as for G1 |
| infinity | 0xc0 then zeros | 0xc0 then zeros |

Legacy `FromBytes` skips `CheckValid` (the subgroup check) and does not throw on relic errors.

Wrapper rules (bls.h:43‑233):
- All‑zero bytes give an invalid (null) object.
- A decoded value equal to the default (infinity) becomes invalid.
- `Unserialize` round‑trips the bytes with `CheckMalleable`. If the round trip fails under the expected scheme but succeeds under the opposite one, the object becomes **invalid without throwing**. If neither works, it throws "malleable BLS object".
- `CBLSId(uint256)` uses the raw 32 bytes; threshold code reads them big‑endian (`bn_read_bin`).
- Secret keys are scheme‑independent.

##### Secure aggregation (`AggregateSecure` / `VerifySecure`, schemes.cpp:196‑256), used for `membersSig`
1. Sort the pubkeys by their serialized bytes, legacy or basic according to the scheme in use.
2. `pkHash = SHA256(concatenation)`.
3. `t_i = SHA256(BE32(i) || pkHash) mod r`.
4. Signature = `Σ t_i · sig_i`; verify against `Σ t_i · pk_i`.

##### Threshold (threshold.cpp)
- Polynomial evaluation by Horner over the coefficient vector (`vec[0]` is the constant term), at `x = BE(id bytes) mod r`. At least 2 coefficients are required.
- Lagrange interpolation at 0, with at least 2 shares.

##### BLS‑IES (bls_ies.cpp)
- Ephemeral sk; `ephemeralPubKey` (serialized with the global flag) and a random `ivSeed`.
- `IV_i = H^i(ivSeed)`, i.e. double‑SHA256 applied i times; the first 16 bytes are used.
- Symmetric key = the first 32 bytes of the **basic** serialization of `ecdh = sk · PK`.
- Cipher is AES‑256‑CBC **without padding** (`AES256CBCEncrypt(key, iv, false)`). The plaintext is the 32‑byte secret key, giving a 32‑byte blob.

##### Worker and lazy objects
- `CBLSWorker` (bls_worker.cpp) provides `GenerateContributions`, `BuildQuorumVerificationVector` (coefficient‑wise sum), `VerifyContributionShares` (batch via aggregation), `VerifyVerificationVector` (all valid and unique), plus caches keyed by `H(memberIndexes)`.
- `CBLSBatchVerifier` verifies by message hash, then by source, then per message.
- `CBLSLazyWrapper` keeps the raw bytes and the scheme flag; its hash is `H(raw bytes)`.

##### Test vectors
- src/dashbls/src/test.cpp: IETF vectors (line 319), Chia vectors (335), Schemes (1062), Legacy HD (1325), Threshold (1439), CheckValid and CheckValid for Legacy (1519, 1574). About 72 hard‑coded hex strings.
- src/test/bls_tests.cpp: mostly property tests; includes `v19_boundary_validation_failure_restores_bls_scheme`.
- src/test/evo_islock_tests.cpp: real‑data islock deserialization (line 177) and request‑id tests.
- src/test/llmq_hash_tests.cpp, llmq_commitment_tests.cpp, llmq_snapshot_tests.cpp, llmq_chainlock_tests.cpp, llmq_utils_tests.cpp.
- src/test/fuzz/dkg_message_framing.cpp.

---

#### 9. Other LLMQ signing users

**EHF (DIP‑23)**, llmq/ehf_signals.cpp and evo/mnhftx.cpp:
- Uses `llmqTypeMnhf`, request id `H(pair(std::string "mnhf", int64 versionBit))`.
- The signer picks the quorum with `SelectQuorumForSigning(tip - 8, id)` and sets `signal.quorumHash`.
- msgHash = txid of the MNHF tx (type 7, version 3) with a null sig.
- Validation (`CheckMNHFTxImpl`, mnhftx.cpp:106‑156): the quorum hash must be on the active chain, `GetQuorum` must succeed (no recency check), then `VerifyInsecure(quorumPK, SignHash(type, quorumHash, id, msgHash))`.
- Signing is only attempted inside the deployment's start/timeout window and for bits not yet signaled.

**Asset unlock** (evo/assetlocktx.cpp:106‑149):
- Type is `llmqTypePlatform`.
- The quorum must be among `ScanQuorums(pindexPrev, activeCount + 1)`, otherwise `bad-assetunlock-too-old-quorum` (TX_BAD_SPECIAL).
- `requestedHeight <= tip < heightToExpiry`.
- id = `H(pair("plwdtx", u64 index))`; msgHash = `H(tx with quorumSig nulled)`, computed over the full serialization.

**Platform signing**: RPCs `quorum sign` and `quorum platformsign` (rpc/quorums.cpp:521‑662). Platform types allow a different msgHash for the same id.

---

#### 10. QRInfo and snapshots

Snapshot DB: evodb raw key `("llmq_S", H(pair(type, cycleBaseHash)))`. It is written outside the block transaction, so it is not reverted on reorg; it is keyed by block hash.

`CQuorumSnapshot` serialization: `int32 mnSkipListMode`, CompactSize n, fixed bitset `activeQuorumMembers` (`ceil(n/8)` bytes, LSB first), CompactSize m, then m `int32` skip entries.

`getqrinfo` (`CGetQuorumRotationInfo`): `vector<uint256> baseBlockHashes` (at most 4096), `blockRequestHash`, `bool extraShare`.

`qrinfo` (`CQuorumRotationInfo`), in order:
1. `snap(H-C)`, `snap(H-2C)`, `snap(H-3C)`
2. `mnListDiffTip`, `mnListDiffH`
3. `diff(H-C)`, `diff(H-2C)`, `diff(H-3C)`
4. `bool extraShare`, then `[snap(H-4C), diff(H-4C)]` if set
5. `vector<CFinalCommitment> lastCommitmentPerIndex`
6. `vector<CQuorumSnapshot>`
7. `vector<CSimplifiedMNListDiff>`

Construction (snapshot.cpp:72‑265) always uses the IS type:
- H is the cycle of `blockRequestHash`; the diffs target each cycle's work block.
- Diff bases are the highest requested base at or below the target.
- Extra snapshots are taken for the 3 prior cycles of each `lastCommitmentPerIndex` quorum.
- Peers below version 70236 (`EFFICIENT_QRINFO_VERSION`) get the legacy construction: no dedup of bases, and different diff‑base threading. Its output must stay byte‑identical.

---

#### 11. P2P summary

| cmd | Handler |
|---|---|
| qfcommit | blockprocessor.cpp |
| qcontrib / qcomplaint / qjustify / qpcommit / qwatch | net_dkg.cpp |
| qsigsesann / qsigsinv / qgetsigs / qbsigs / qsigshare / qsigrec | net_signing.cpp |
| qsendrecsigs | net_processing |
| qgetdata / qdata | net_quorum.cpp |
| clsig | chainlock |
| isdlock | net_instantsend |
| getqrinfo / qrinfo | net_processing:5672 |

Inv types: 21 (final commitment) and 23‑31 as listed above. Protocol version 70242; `MIN_MASTERNODE_PROTO_VERSION` 70242.

---

#### 12. Pitfalls for a Rust reimplementation

1. **The global BLS scheme flag decides bytes.** Default (de)serialization and `VerifyInsecure` use the per‑block flag (legacy iff block height < V19Height). Commitment field encoding uses explicit version wrappers. `BuildCommitmentHash` hashes `pubKey` under the global flag; so do `quorumVvecHash`, the post‑V20 modifier's `bestCLSignature`, and the IES `ephemeralPubKey`. The DKG vvec is persisted as basic. Model the scheme as explicit context, never as a global.
2. **Opposite‑scheme bytes decode to an invalid object, not an error.** All‑zero bytes give null; garbage throws. A CbTx `bestCLSignature` that is invalid is treated as null by `CheckCbTxBestChainlock` and `GetNonNullCoinbaseChainlock`. Legacy decoding skips the subgroup check.
3. **The legacy hash‑to‑G2 (`ep2_map_legacy`) is non‑standard.** No mainstream crate implements it; port legacy.cpp exactly, including SW encoding and the cofactor method. The G2 sign bit compares y.c1 only.
4. **Score sort tie‑break:** collateral outpoint descending in the final order. Scores are single SHA256 interpreted little‑endian. Rotation `CalculateQuorum` on an `MNList` uses valid‑only MNs, but the snapshot bit vector length is `total()` including banned MNs.
5. **Rotation needs snapshots that are created as a side effect** of computing members at each cycle base (`PreComputeQuorumMembers` in ProcessBlock). If too few MNs are enabled, no snapshot is stored. `AddMN` uniqueness exceptions silently skip MNs. The skip‑list encoding has a quirk around a first index of 0. Replicate all of this, including the V19 `skipRemovedMNs` rule and the testnet exception.
6. **Modifier and MN list V20 checks differ.** Non‑rotated: the list comes from `base - 8` if V20 is active after base. The modifier is new‑style if V20 is active after `base - 8`.
7. **Rotated signer selection** uses `(GetUint64(3) >> (64 - n - 1)) & mask`, not the top n bits. Non‑rotated selection sorts `uint256` by memcmp of little‑endian bytes.
8. **ScanQuorums snaps to the mining window** and uses mined height ≤ start. CL, recsig, asset‑unlock and EHF validity depend on this. CL verification uses `chain[clHeight - 8]`.
9. **Commitment block rules:**
   - The exact count of commitments is enforced per enabled type, using the type set at `pprev`.
   - Null commitments skip the version check and, for rotated types, have no per‑index uniqueness check (count only).
   - `quorumIndex == base.h % C` is checked even for non‑indexed versions.
   - Bitsets reject padding bits.
   - The tx payload `nVersion` must be 1 and `nHeight` must equal the block height.
   - The commitment hash is of `(qc, minedBlockHash)` serialization for the duplicate check.
10. **Threshold ids** are `BE_int(raw uint256 bytes) mod r`, i.e. the reverse of the display hex.
11. **Request‑id prefixes are length‑prefixed strings:** "clsig" + int32, "inlock" + COutPoint, "islock" + vector<COutPoint>, "mnhf" + int64, "plwdtx" + uint64. `SignHash` is double‑SHA256.
12. **IS verification tries two windows** (offset 0, then dkgInterval) with special `nSignHeight` logic. The `cycleHash` must be at a cycle boundary.
13. **ChainLocks are not persisted.** They are rebuilt from coinbase CLs. Header and block conflicts with the best CL are consensus‑rejected locally, so behaviour depends on node state; match `HasConflictingChainLock` semantics.
14. **DKG timing is cooperative**, driven by block height and spork‑dependent PoSe checks. Members are judged (badConnection) on relay‑tx and compact‑filter service flags of MN connections, so a Rust MN must advertise them.
15. **The IES symmetric key** is the basic encoding of the ECDH point truncated to 32 bytes, used with AES‑CBC without padding. `IV_i` is `ivSeed` hashed i times with double‑SHA256, first 16 bytes.
16. **Mainnet V19Height (1899072) is cycle‑aligned**, so no quorum straddled the scheme switch. Keep `version = f(base)` and `flag = f(processing block)` separate anyway, especially for devnet and regtest, where V19 is at height 1‑2.

### A.5 Governance and CoinJoin

*Section numbers below (§1, §2, …) are local to A.5.*

These notes cover Dash Core at `/work/home/agent/workspace/dash`, branch develop, HEAD `15db478750`. Line numbers refer to that tree.

**Big-picture findings that affect a Rust design:**
- **V24 adds CoinJoin "rebalance" sessions.** These promote or demote coins between adjacent denominations. They are gated by `DEPLOYMENT_V24` and `COINJOIN_REBALANCE_VERSION=70241`. Rebalance transactions are unbalanced (input count ≠ output count). This is new protocol surface you won't find in older documentation.
- **Superblock validity depends on local governance state.** The full check is skipped for chainlocked blocks and for nodes that aren't synced.
- **The proposal fee is a flat 1 DASH.** Older collateral transactions needed 5 DASH; the 1 DASH rule was gated on DIP0024, and that gate was later hardened away.

---

#### 1. File inventory

| Path | Lines | What it does |
|---|---|---|
| src/governance/common.h | 88 | `GovernanceObject` enum and the `Governance::Object` POD (wire format and hash) |
| src/governance/common.cpp | 69 | `Object::GetHash`, `ToJson` |
| src/governance/object.h | 371 | `CGovernanceObject`, vote records, constants (fee, confirmations) |
| src/governance/object.cpp | 1034 | Proposal validator, collateral check, vote processing, sentinel flags |
| src/governance/vote.h | 215 | `CGovernanceVote` wire format, signal and outcome enums |
| src/governance/vote.cpp | 292 | Vote hash, signature checks (ECDSA voting key / BLS operator key) |
| src/governance/votedb.h | 97 | `CGovernanceObjectVoteFile` (per-object vote list) |
| src/governance/votedb.cpp | 134 | Add, remove-old, remove-invalid votes |
| src/governance/governance.h | 426 | `GovernanceStore` (disk format), `CGovernanceManager`, `CRateCheckBuffer` |
| src/governance/governance.cpp | 1229 | Object and vote processing, orphans, cleanup, approved proposals |
| src/governance/net_governance.h | 40 | `NetGovernance` P2P handler |
| src/governance/net_governance.cpp | 303 | govsync / govobj / govobjvote handling, getdata, scheduler |
| src/governance/signing.h | 60 | `GovernanceSigner` (masternode-only) |
| src/governance/signing.cpp | 308 | Trigger creation and trigger funding votes by the active masternode |
| src/governance/superblock.h | 184 | `CSuperblock`, `CGovernancePayment`, `SuperblockManager` |
| src/governance/superblock.cpp | 679 | Payment limit, trigger parsing, coinbase validation |
| src/rpc/governance.cpp | 1052 | `gobject *`, `voteraw`, `getgovernanceinfo`, `getsuperblockbudget` |
| src/masternode/payments.cpp | 474 | `IsBlockValueValid` / `IsBlockPayeeValid` / `FillBlockPayments` (superblock hooks) |
| src/node/sync_manager.cpp | 358 | Masternode-sync state machine; governance sync requests |
| src/flat-database.h | 209 | `CFlatDB<T>` (governance.dat format) |
| src/coinjoin/common.h | 284 | Denominations, collateral amounts, promotion/demotion math, `CTxDSIn` |
| src/coinjoin/common.cpp | 26 | `DenominationToString` |
| src/coinjoin/coinjoin.h | 575 | Message classes, pool enums, queue manager, `CDSTXManager` |
| src/coinjoin/coinjoin.cpp | 768 | Signatures, `IsValidInOuts`, collateral validation, DSTX structure checks |
| src/coinjoin/server.h | 171 | `CCoinJoinServer` (masternode side) |
| src/coinjoin/server.cpp | 1314 | dsa / dsq / dsi / dss handling, session lifecycle, fee charging |
| src/coinjoin/client.h | 303 | `CCoinJoinClientSession`, `CCoinJoinClientManager` |
| src/coinjoin/client.cpp | 2521 | Auto-denominate, collateral and denom creation, joining, signing |
| src/coinjoin/options.h | 99 | `-coinjoin*` defaults and limits |
| src/coinjoin/options.cpp | 88 | Option init and clamping |
| src/coinjoin/util.h | 147 | `CKeyHolderStorage`, `CTransactionBuilder` |
| src/coinjoin/util.cpp | 358 | Transaction builder fee and size maths |
| src/coinjoin/walletman.h | 69 | `CJWalletManager` interface |
| src/coinjoin/walletman.cpp | 339 | Per-wallet client managers; client-side DSQUEUE processing |
| src/coinjoin/interfaces.cpp | 67 | `interfaces::CoinJoin::Loader` |
| src/wallet/coinjoin.h | 42 | Balance helpers |
| src/wallet/coinjoin.cpp | 671 | Salt, rounds, `IsFullyMixed`, denom selection |
| src/rpc/coinjoin.cpp | 553 | `coinjoin *`, `coinjoinsalt *`, `getcoinjoininfo` |

---

#### 2. Governance

##### 2.1 Object types

`src/governance/common.h:25-29`: `enum class GovernanceObject : int { UNKNOWN=0, PROPOSAL=1, TRIGGER=2 }`.

- The old sentinel WATCHDOG type (3) was removed long ago (commits #1079/#1166/#1167). Today any type other than 1 or 2 fails `IsValidLocally` with "Invalid object type" (`object.cpp:767-770`).
- `GetMinCollateralFee` returns `MAX_MONEY` for unknown types (`object.cpp:774-788`).
- The type is not taken from the wire field. `LoadData()` parses `vchData` as JSON and sets `m_obj.type = obj["type"]` (`object.cpp:634-656`). If parsing fails, `fUnparsable=true` and the object is invalid.

##### 2.2 Object fields, wire format and hashes

`Governance::Object` (`common.h:49-84`), serialized in this order:

| # | Field | Type | Notes |
|---|---|---|---|
| 1 | hashParent | uint256 | 0 = root |
| 2 | revision | int32 | |
| 3 | time | int64 | creation time (unix seconds) |
| 4 | collateralHash | uint256 | proposal fee txid; 0 for triggers |
| 5 | vchData | vector<u8> | JSON bytes; `gobject` RPCs take them as hex |
| 6 | type | int32 enum | serialized but overwritten by `LoadData` |
| 7 | masternodeOutpoint | COutPoint | null for proposals; signer's collateral for triggers |
| 8 | vchSig | vector<u8> | omitted when `SER_GETHASH` |

Three different hashes are used:

- **Object hash** (`GetHash`, `common.cpp:111-127`). This is the identity used in invs, votes and the collateral OP_RETURN. It does **not** match the serialization. It is double-SHA256 over:
  `hashParent ‖ int32 revision ‖ int64 time ‖ ser_string(HexStr(vchData)) ‖ COutPoint ‖ uint8 0 ‖ uint32 0xffffffff ‖ ser_vec(vchSig)`.
  - `vchData` is hashed as its lowercase hex string with a compact-size prefix.
  - `collateralHash` is excluded on purpose.
  - `vchSig` is included, so a signed trigger's hash depends on its signature.
- **Signature hash** (`object.cpp:551-554`). `SerializeHash(obj)` with `SER_GETHASH`, i.e. fields 1–7 without `vchSig`.
- **Data hash** (`object.cpp:543-549`). `Hash(ser_string(hex(vchData)))`. Masternodes use it to find identical triggers (`FindGovernanceObjectByDataHash`).

**Disk serialization** (`SER_DISK`, `object.h:451-473`) appends:
- `int64 nDeletionTime`
- `bool fExpired`
- `map<COutPoint, vote_rec_t> mapCurrentMNVotes`, where `vote_rec_t` is `map<int, vote_instance_t{int32 outcome, int64 last_update, int64 nCreationTime}>`
- `CGovernanceObjectVoteFile{int32 nMemoryVotes, list<CGovernanceVote>}`

Cached flags (funding, valid, delete, endorsed, dirty) are not persisted; they are recomputed after load.

##### 2.3 Collateral transaction (proposals only)

Constants (`object.h:243-247`):

| Constant | Value |
|---|---|
| `GOVERNANCE_PROPOSAL_FEE_TX` | 1 COIN |
| `GOVERNANCE_FEE_CONFIRMATIONS` | 6 |
| `GOVERNANCE_MIN_RELAY_FEE_CONFIRMATIONS` | 1 |
| `GOVERNANCE_UPDATE_MIN` | 1 h |
| `GOVERNANCE_FILTER_FP_RATE` | 0.001 |

**Fee history.** Before #5344 (commit `04a31c76e0`) the fee was `GOVERNANCE_PROPOSAL_FEE_TX_OLD` = 5 DASH, and the reduced 1 DASH fee depended on DIP0024 being active at the collateral's block. Commit #4241 (`2204c85370`) introduced the reduction; #5344 hardened it to a flat 1 DASH. Script (P2SH) payment addresses also became unconditionally allowed in #5344. The `gobject prepare` comment saying "5 DASH" is stale (`rpc/governance.cpp:186`).

`IsCollateralValid` (`object.cpp:790-878`) is policy, not consensus:
1. Look the transaction up via `g_txindex`. This is why `-txindex` is mandatory unless `-disablegovernance` is set or on regtest (`init.cpp:1444-1448`).
2. It must be mined (block hash ≠ 0) and have at least one output.
3. Every output must be P2PKH or unspendable.
4. At least one output must have `scriptPubKey == OP_RETURN <push 32 bytes = object hash>` and `nValue >= 1 COIN`.
5. Confirmations are counted in the active chain. At ≥ 6 the collateral is valid. At 1–5, `fMissingConfirmations=true` and the object is postponed (`mapPostponedObjects`, re-checked on each tip). At 0 it is rejected.

Wallet side: `GenBudgetSystemCollateralTx` (`wallet/spend.cpp:1296-1313`) builds one recipient `OP_RETURN <hash>` for `amount`, with an optional forced single input.

##### 2.4 Proposal JSON schema and validation

`governance::ValidateProposal(hex, err, fCheckExpiration=true, fAllowScript=true)` is at `object.cpp:1000-1033`. Checks run in this order and stop at the first failure. The error string is a list of messages joined with `;`.

| Step | Rule (source) |
|---|---|
| Parse | Hex-decoded bytes ≤ `MAX_DATA_SIZE=512` (`object.cpp:33, 273-300`). Must be a JSON object. |
| type | JSON number == 1 (`:81-95`) |
| name | String, 1..40 chars (`MAX_NAME_SIZE`). After lowercasing, only `-_abcdefghijklmnopqrstuvwxyz0123456789` (`:97-125`) |
| start_epoch, end_epoch | Both JSON numbers (int64). `end > start`. If `fCheckExpiration`, `end_epoch > GetAdjustedTime()` (`:127-154`) |
| payment_amount | JSON number read as a double, must be > 0. No budget ceiling is checked (`:156-175`) |
| payment_address | String, no whitespace, `DecodeDestination` valid. P2SH rejected only if `!fAllowScript`, which is never the case now (`:177-204`) |
| url | String, no whitespace, length ≥ 4, `CheckURL` (mimics Python urlparse: `[`/`]` must be balanced in the netloc) (`:206-271`) |

Extra fields are ignored.

`IsValidLocally` for a proposal calls `ValidateProposal` with `fCheckExpiration=false`. Expired proposals are allowed to exist; `CheckAndRemove` re-runs validation with the expiration check and marks failing proposals for deletion (`governance.cpp:474-479`).

Subtlety: the trigger builder re-parses `payment_amount` with `ParsePaymentAmount(getValStr())` (`superblock.cpp:22-64`). That function allows only digits and `.`, at most one `.`, not as the first character, at most 8 decimals, and a value within `MoneyRange`. A proposal whose amount is written as `1e3` passes validation but is silently skipped when building triggers.

##### 2.5 Votes

Enums (`vote.h:22-39`):
- Outcome: `NONE=0, YES=1, NO=2, ABSTAIN=3` (`UNKNOWN=4` is a sentinel).
- Signal: `NONE=0, FUNDING=1, VALID=2, DELETE=3, ENDORSED=4` (`UNKNOWN=5`).

RPC strings: "yes/no/abstain" and "funding/valid/delete/endorsed".

**Wire format** (`vote.h:190-207`):
`COutPoint masternodeOutpoint ‖ uint256 nParentHash ‖ int32 outcome ‖ int32 signal ‖ int64 nTime ‖ vector vchSig`.
- On network reads, `vchSig` length must be exactly 65 (compact ECDSA) or 96 (BLS) bytes, capped before allocation. Anything else throws, and the peer gets misbehaving +100 (`net_governance.cpp:247-252`).

**Vote hash** (identity / inv; `vote.cpp:145-156`). Again not the serialization order. Double-SHA256 of:
`COutPoint ‖ uint8 0 ‖ uint32 0xffffffff ‖ parentHash ‖ int32 signal ‖ int32 outcome ‖ int64 nTime`.
Note that signal comes before outcome here, but outcome comes before signal in the serialization.

**Signature hash.** `SerializeHash(vote)` with `SER_GETHASH`, i.e. the serialization without `vchSig` (outcome before signal).

**Which key signs** (`object.cpp:458`, `votedb.cpp:88`, `vote.cpp:211-250`):

| Case | Key | Scheme |
|---|---|---|
| Object is PROPOSAL **and** signal is FUNDING | voting key (`keyIDVoting`, secp256k1) | Mainnet, devnet, regtest: `CMessageSigner::VerifyMessage` = recover a compact sig over `DSHA256(ser_string("DarkCoin Signed Message:\n") ‖ ser_string(msg))`, where `msg = "<txid>-<n>|<parentHash hex>|<signal int>|<outcome int>|<nTime>"` (`vote.h:166-172`, `util/message.cpp:25`). Testnet only ("hardened spork6"): compact sig directly over the signature hash (`CHashSigner::VerifyHash`, `vote.cpp:175-186`). Wallet signing mirrors this (`wallet.cpp:2431-2456`). |
| Every other signal on proposals, and all signals on triggers (including FUNDING) | operator BLS key (`pubKeyOperator`) | `CBLSSignature::VerifyInsecure(pubKey, sigHash, specificLegacyScheme=false)`, i.e. the basic scheme. Masternodes sign with `SignBasic` (`active/masternode.cpp:296-302`). |

- For an orphan vote whose parent is unknown, `IsValidForUnknownParent` accepts FUNDING if it verifies with the voting key; otherwise it requires the operator key (`vote.cpp:244-250`).
- `IsValid` also rejects votes more than 1 h in the future (`GetAdjustedTime()+1h`) and out-of-range enum values, and requires the masternode to be in the tip list (any MN, not only valid ones).
- Verification results are memoized per (key, signature hash, signature) (`vote.h:97-123`). This is an optimization only.

**`CGovernanceObject::ProcessVote` order of checks** (`object.cpp:375-481`):
1. Already-known vote hash → no error.
2. MN not found → permanent error, penalty 20.
3. Signal NONE → warning. Out-of-range signal → penalty 20.
4. Obsolete (`nTime` older than the stored instance) → reject.
5. Equal `nTime` with a different outcome: the vote with the lower outcome value loses (deterministic tie-break).
6. **Rate limit:** if rate checks are enabled and the last update for this (MN, signal) was less than 1 h ago → temporary error.
7. Signature check → penalty 20 on failure.
8. Store, then `mn_metaman.AddGovernanceVote`, then ZMQ `NotifyGovernanceVote`.

`votedb` keeps only the newest vote per (MN, parent, signal) (`votedb.cpp:103-119`).

**Tallies** (`object.cpp:880-997`):
- Vote weight is `GetMnType(dmn->nType).voting_weight`: Regular = 1, Evo = 4 (`evo/dmn_types.h:33-39`). MNs not in the tip list count 0.
- `AbsoluteYes = Yes − No`.
- `UpdateSentinelVariables`, with `W = tip_mn_list.GetCounts().m_valid_weighted`:
  - `req = max(nGovernanceMinQuorum, W/10)`
  - `delReq = max(nGovernanceMinQuorum, 2W/3)`
  - `fCachedFunding = absYes(FUNDING) ≥ req`
  - `absYes(DELETE) ≥ delReq` → mark for deletion
  - `fCachedEndorsed = absYes(ENDORSED) ≥ req`
  - `fCachedValid = absNo(VALID) < req`

**Invalidating votes after key changes** (`governance.cpp:1150-1196`). Once DIP3 is enforced and the node is synced, on each tip the node diffs the MN list against `lastMNListForVotingKeys`. For MNs whose voting key or operator key changed, or that were removed, it re-validates their votes and drops the invalid ones. Separately, `CheckAndRemove` drops all votes from MNs flagged "dirty" by `mn_metaman`, i.e. no longer in the list (`ClearMasternodeVotes`).

##### 2.6 Rate limiting and orphans

- **Trigger object rate** (`governance.cpp:699-788`). Applies to triggers only, and only when synced and rate checks are enabled.
  - Creation time must lie in `[now − 2·cycleSeconds, now + 1h]`.
  - Each MN outpoint has a `CRateCheckBuffer` (circular buffer of 5 timestamps, `RATE_BUFFER_SIZE=5`, `governance.h:63-154`).
  - The rate is `count/(max−min)`, applied only once the buffer is full; equal min and max means rate = 1e10.
  - The object is rejected if `rate ≥ 2·1.1/cycleSeconds`.
  - `fStatusOK` lets a node skip the check until signatures are verified (`fRateCheckBypassed`). The check is then re-run with `fForce` after `IsValidLocally` (`governance.cpp:239-260`).
  - Triggers whose time is within 60 s of the +1 h limit are put in `setAdditionalRelayObjects` and re-relayed once they are "ready" (`:711-714, 890-920`).
- **Vote rate.** At most 1 update per (MN, object, signal) per hour (`GOVERNANCE_UPDATE_MIN`).
- **Orphan votes** (`governance.cpp:820-839`).
  - A vote for an unknown object is checked with `IsValidForUnknownParent`; invalid → penalty 20.
  - Valid orphans go into `cmmapOrphanVotes` (`CacheMultiMap<uint256, OrphanVote>`, max 1,000,000 entries) with a 10 min expiry, and the node immediately sends that peer `govsync <hash> <empty bloom>` (`net_governance.cpp:300-305`).
  - When the parent arrives, `CheckOrphanVotes` replays them with rate checks off (`:277-304`).
  - Every 5 min the scheduler requests every orphan parent from all relay-capable peers, then runs `CheckAndRemove` (`net_governance.cpp:84-107`).

##### 2.7 Object lifecycle (`CGovernanceManager`)

- **`ProcessObject`** (`governance.cpp:227-275`):
  1. Ignore if already present in `mapObjects`, `mapPostponedObjects` or `mapErasedGovernanceObjects`.
  2. Rate check.
  3. `IsValidLocally(checkCollateral=true)`.
  4. If not valid but `fMissingConfirmations` → postpone. If invalid otherwise → return false; the caller adds misbehaving +20.
  5. Otherwise `AddGovernanceObjectInternal`.
- **`AddGovernanceObjectInternal`** (`:306-369`): update sentinel flags, re-validate, insert. Triggers are registered with `SuperblockManager::AddTrigger`; if that fails, the object is prepared for deletion. Then: relay inv, update the rate buffer, bump sync, replay orphans, emit ZMQ.
- **`CheckAndRemove`** (`:384-497`). Runs only when the blockchain is synced.
  - Updates dirty objects.
  - Deletes objects with (`fCachedDelete` or `fExpired`) once `now − nDeletionTime ≥ 10 min` (`GOVERNANCE_DELETION_DELAY`).
  - Erased hashes are remembered in `mapErasedGovernanceObjects`: forever (`INT64_MAX`) for proposals; for triggers until `creation + 2·cycleSeconds + 10 min`.
- **`UpdatedBlockTip`** (`:1095-1119`): `RemoveInvalidVotes`, `CheckPostponedObjects`, `ExecuteBestSuperblock` (marks the trigger executed).
- **Cache sizes:** `MAX_CACHE_SIZE = 1,000,000` for `cmapVoteToObject` and `cmmapOrphanVotes` (`governance.h:179`).

##### 2.8 Superblock parameters

From `chainparams.cpp`:

| Network | nSuperblockCycle | nSuperblockStartBlock | nSuperblockMaturityWindow | nGovernanceMinQuorum | nGovernanceFilterElements | nBudgetPaymentsStartBlock / Cycle / Window | V20Height | MN_RRHeight |
|---|---|---|---|---|---|---|---|---|
| main | 16616 | 614820 (hash `0000000000020cb2…8aa3`, consensus-checked `bad-sb-start`, `validation.cpp:2617-2622`) | 1662 | 10 | 20000 | 328008 / 16616 / 100 | 1987776 | 2128896 |
| testnet | 24 | 4200 | 8 | 1 | 500 | 4100 / 50 / 10 | 905100 | 1066900 |
| devnet | 24 | 4200 | 8 | 1 | 500 | 4100 / 50 / 10 | 2 | 2 |
| regtest | 20 | 1500 (`-budgetparams` overrides) | 10 | 1 | 100 | 1000 / 50 / 10 | DIP0003Height | V20Height |

Superblock helpers (`superblock.cpp`):
- `IsValidBlockHeight(h)`: `h ≥ nSuperblockStartBlock && h % cycle == 0` (`:119-124`).
- `GetNearestSuperblocksHeights`: `:126-143`.

##### 2.9 Payment limit, V20 and MN_RR

`GetPaymentsLimit(h)` (`superblock.cpp:145-163`):
- Returns 0 if `h` is not a superblock height.
- Otherwise `GetSuperblockSubsidyInner(nBits, h−1, fV20 = h ≥ V20Height) × nSuperblockCycle`, where `nBits = powLimit.GetCompact()` if `fPowAllowMinDifficultyBlocks`, else 1.

`GetBlockSubsidyHelper` (`validation.cpp:1810-1871`):
- `nSuperblockPart = nSubsidy / (fV20 ? 5 : 10)` when `nPrevHeight > nBudgetPaymentsStartBlock`.
- So the treasury is 10% before V20 and 20% after.
- From V20 on, `nSubsidyBase` is fixed at 5, so `nBits` no longer matters.

MN_RR does **not** change the superblock budget. It changes the masternode share of the remaining block reward to `blockValue·3/4` (`payments.cpp:95-100`) and adds the Platform credit-pool share.

##### 2.10 Trigger creation (masternode only, `governance/signing.cpp`)

`GovernanceSigner::UpdatedBlockTip(pindex)` (`:299-308`):

1. **`CreateSuperblockCandidate(h)`** (`:38-119`). Runs only if governance is loaded, the node is synced, `h % cycle ≥ cycle − maturityWindow`, and the node has not already voted YES on a trigger this cycle.
   - `GetApprovedProposals`: proposals with `absYes(FUNDING) ≥ max(minQuorum, W/10)`, sorted by absYes descending, ties broken by hash descending (`governance.cpp:1198-1229`).
   - The superblock time is estimated as `now + (nextSB − h)·2.62·60`.
   - The budget is `GetPaymentsLimit(nextSB)`.
   - Proposals are added greedily, skipping any that would exceed the budget, and skipping any where the superblock time falls outside `[start_epoch − 2h, end_epoch + 2h]` (`GOVERNANCE_FUDGE_WINDOW`).
   - Payments are sorted by proposal hash descending.
2. **`CreateGovernanceTrigger`** (`:121-179`).
   - Build the object `(parent=0, revision=1, time=GetAdjustedTime(), collateral=0, data=sb.GetHexStrData())`.
   - If an object with the same **data hash** already exists, support that one instead of creating a duplicate.
   - Otherwise, create it only if this MN is `GetProjectedMNPayees(tip).front()`. Set the outpoint, sign with BLS basic over the signature hash, validate locally, rate-check, then `AddGovernanceObject`.
3. **`VoteGovernanceTriggers`** (`:181-271`). Vote FUNDING-YES (operator BLS) on the chosen trigger, and FUNDING-NO on every other active future trigger this MN hasn't already voted on.

The trigger JSON is a byte-exact template. This matters because masternodes deduplicate triggers by data hash (`superblock.cpp:389-413`):
```
{"event_block_height": H, "payment_addresses": "a1|a2", "payment_amounts": "x.xxxxxxxx|…", "proposal_hashes": "h1|h2", "type":2}
```
- There is a space after each `:` except in `"type":2`.
- Amounts use `ValueFromAmount().write()` (8 decimals).

Trigger parsing (`CSuperblock(const CGovernanceObject&)`, `:75-105`):
- Requires `event_block_height`, `payment_addresses`, `payment_amounts` and `proposal_hashes`, each `|`-separated with equal counts and at least 1 entry.
- Addresses must decode; amounts go through `ParsePaymentAmount`; hashes through `ParseHashStr`.

##### 2.11 How superblocks are validated in blocks

`ConnectBlock` (`validation.cpp:2840-2848`) computes `check_superblock`:
- `NoCheck` if the block is already chainlocked (`best CL height ≥ h`) or the node is not masternode-synced (`evo/chainhelper.cpp:41-52`).
- Otherwise `DisallowDuplicates` if V24 is active, else `AllowDuplicates`.

`IsBlockValueValid` (`payments.cpp:313-390`). Failure returns `bad-cb-amount`, and the peer is not punished.
- Below `nBudgetPaymentsStartBlock`: coinbase ≤ reward.
- Old-budget window: special handling (`:262-301`).
- Not a superblock height: coinbase ≤ blockReward.
- Superblock height: coinbase must be ≤ `blockReward + GetPaymentsLimit(h)` (always enforced).
- If governance isn't loaded or the check is `NoCheck`, accept.
- If `!IsSuperblockTriggered` (no trigger at that height has `fCachedFunding`), coinbase must be ≤ blockReward.
- Otherwise `IsValidSuperblock`.

`IsBlockPayeeValid` (`payments.cpp:392-442`): checks the masternode payment, then, if triggered, `IsValidSuperblock` with `blockReward = subsidy + fees`.

`SuperblockManager::GetBestSuperblock` (`superblock.cpp:556-576`): among triggers with `event_block_height == h`, pick the one with the strictly highest `absYes(FUNDING)`, which must be > 0. The `std::map` iteration order (by hash) decides ties.

`CSuperblock::IsValid` (`:248-337`):
- Valid height.
- `vout.size() ≥ #payments`.
- `Σpayments ≤ GetPaymentsLimit`.
- `GetValueOut ≤ blockReward + Σpayments`.
- Each payment, in order, must match `(script, amount)` exactly, scanning forward. From V24 on, the scan starts at `lastIdx+1` so duplicate payments need distinct outputs; before V24 it starts at `max(lastIdx, 0)`.

Miners use `FillBlockPayments` (`payments.cpp:444-474`), which appends MN outputs and then superblock outputs.

Trigger expiry (`IsExpired`, `:339-375`): Executed triggers are kept for `cycle` blocks, Valid ones for `min(576, cycle)`, others for `min(24, cycle)`. On non-mainnet networks a trigger also expires if `h + 2·cycle < event height`.

##### 2.12 Sync and P2P protocol

| Message | Payload | Handling |
|---|---|---|
| `govsync` (MNGOVERNANCESYNC) | `uint256 nProp, CBloomFilter` | Ignored until synced. The bloom filter must satisfy `IsWithinSizeConstraints`, else misbehaving 100. `nProp==0` is a full sync: reply `ssc(MASTERNODE_SYNC_GOVOBJ=10, n)` plus an inv for every syncable object (not deleted or expired). `nProp≠0` with an empty bloom is an "object fetch": send `inv(MSG_GOVERNANCE_OBJECT)` if we have it; not rate-tracked. `nProp≠0` with a bloom is a vote sync: reply `ssc(11, n)` plus an inv for each valid vote not in the bloom. Full syncs and known-object vote syncs are limited by netfulfilled entries `"govsync"` or `"govsync-votes-<hash>"`; a repeat → misbehaving 20 (`net_governance.cpp:126-209`). |
| `govobj` | `CGovernanceObject` (network serialization = `Governance::Object`) | Accepted only if this peer announced it or we requested it (`PeerConsumeObjectRequest`). `ProcessObject` returning false → +20 (`:211-239`). |
| `govobjvote` | `CGovernanceVote` | Same announced/requested gate. A malformed vote → +100. Exception penalties apply only when synced (`:242-310`). |
| inv types | `MSG_GOVERNANCE_OBJECT=17`, `MSG_GOVERNANCE_OBJECT_VOTE=18` (`protocol.h:508-509`) | `AlreadyHave` = `!ConfirmInventoryRequest`, false until the blockchain is synced. When `-disablegovernance` is set, `AlreadyHave` returns true for everything (`:313-322`). |
| `ssc` (SYNCSTATUSCOUNT) | `int itemID, int count` | Informational only. |

Relay is batched: `m_relay_invs` is flushed every 5 s (1 s on mockable chains) via `PeerRelayInv`.

**Sync state machine** (`node/sync_manager.cpp`, `masternode/sync.h:16-24`). Assets: BLOCKCHAIN=1 → GOVERNANCE=4 → FINISHED=999. The tick runs every 6 s.
- On every peer: send `getsporks` once (netfulfilled `"spork-sync"`).
- In the GOVERNANCE stage: send `govsync(0, empty)` once per peer (`"governance-sync"`), then per-object vote requests (`govsync(hash, bloom of our votes)`), triggers first.
  - Up to 3 peers per hash on real networks (unlimited on regtest).
  - Each (hash, peer) is re-asked at most once per hour.
  - On mainnet, 1 object per node per tick; on other networks `max(1, 2000/enabledMNs)`.
- The stage finishes after a 30 s timeout with no bump, or once all objects have been asked and fewer than `max(0.0001·votes, 6)` new votes arrived in the last tick.
- When finished, every peer is marked `"full-sync"` (`:350-357`).
- Fulfilled requests expire after 1 h on mainnet and 5 min on other networks (`chainparams.cpp:291,485,668,863`). They are persisted in `netfulfilled.dat` (magic `"magicFulfilledCache"`).

##### 2.13 governance.dat

The `CFlatDB` format (`flat-database.h:38-158`) is a `SER_DISK, CLIENT_VERSION` stream:
```
ser_string(magic="magicGovernanceCache") ‖ 4-byte network MessageStart ‖ GovernanceStore ‖ uint256 DSHA256(all preceding bytes)
```

`GovernanceStore` (`governance.h:201-236`):
```
ser_string("CGovernanceManager-Version-16")      // governance.cpp:31
map<uint256,int64> mapErasedGovernanceObjects
CacheMap<uint256,CGovernanceVote>                 // legacy invalid-vote cache, always written empty: uint32 nMaxSize=1000000, list (compact size 0)
CacheMultiMap<uint256,OrphanVote>                 // uint32 nMaxSize, list<{key, OrphanVote{vote, int64 expiration}}>
map<uint256, CGovernanceObject (SER_DISK form)>   // via shared_ptr
map<COutPoint, last_object_rec{CRateCheckBuffer{vector<int64>(5), int32 start, int32 end, bool empty}, bool fStatusOK}>
CDeterministicMNList lastMNListForVotingKeys      // full MN-list serialization (see the evo spec)
```

Load and store behavior:
- A version mismatch on load leaves the store empty and is not an error.
- A bad hash, magic or network causes a load failure, which is fatal at init (`init.cpp:2267-2274`). An incorrect format clears the store and recreates it.
- The file is written in the destructor (`governance.cpp:88-96`).
- The file is not loaded on reindex; it is rewritten empty instead (`init.cpp:2249`).

##### 2.14 Governance RPCs (`src/rpc/governance.cpp`)

| RPC | Behavior |
|---|---|
| `gobject count [json\|all]` | Object, proposal, trigger, other, erased and vote counts (`:41-71`) |
| `gobject deserialize hex` | Hex → JSON string (`:73-96`) |
| `gobject check hex` | `ValidateProposal`. Only proposals are accepted (`:98-142`) |
| `gobject prepare parent rev time hex [use-IS] [outputHash outputIndex]` (wallet) | Rejects triggers. Validates the proposal and `IsValidLocally` without collateral. Builds the collateral transaction, stores `Governance::Object` in the wallet (`g_object`), commits the transaction, returns the txid (`:144-258`) |
| `gobject list-prepared [count=10]` | Wallet's stored objects sorted by time, then hex (`:260-308`) |
| `gobject submit parent rev time hex fee-txid` | Requires the blockchain synced. Rejects triggers. `ValidateProposal`; `IsValidLocally(collateral)` unless only confirmations are missing; `MasternodeRateCheck`. Then postpone and relay, or add. Returns the object hash (`:312-428`) |
| `gobject vote-many hash signal outcome` (wallet) | Signs with every valid MN voting key owned (`ISMINE_SPENDABLE`), then `ProcessVoteAndRelay` (`:514-564`). Always uses the voting key, so non-funding votes and trigger votes made this way fail verification |
| `gobject vote-alias hash signal outcome protx` | Same, for one MN (`:566-618`) |
| `gobject list [signal=valid] [type=all]` / `gobject diff` | Filter by cached flag and type. Output: `GetStateJson` plus funding vote counts. `diff` uses `nTimeLastDiff`, and both update it (`:620-735`) |
| `gobject get hash` | State plus Funding, Valid, Delete and Endorsed results (`:737-771`) |
| `gobject getcurrentvotes hash [txid vout]` | Current tallying votes formatted as `"outpoint:time:outcome:signal:weight"` (`:773-825`) |
| `voteraw collat-txid collat-idx gov-hash signal outcome time sig-base64` | Verifies against the voting or operator key per the rule in 2.5, then process and relay (`:861-942`) |
| `getgovernanceinfo` | `governanceminquorum`, `proposalfee`=1.0, `superblockcycle`, `superblockmaturitywindow`, `lastsuperblock`, `nextsuperblock`, `fundingthreshold`=W/10 (no min-quorum max), `governancebudget` = limit at the next superblock (`:944-991`) |
| `getsuperblockbudget height` | `GetPaymentsLimit` (`:993-1015`) |

Options:
- `-disablegovernance` (default 0, DEBUG_TEST). It is forced on by `-prune` and forbidden on masternodes.
- `-txindex` is required otherwise.
- ZMQ: `-zmqpub{hash,raw}governance{object,vote}`.
- There are no `-govobj*` options.

---

#### 3. CoinJoin

##### 3.1 Denominations and amounts

From `coinjoin/common.h:41-118`:

| Bit (nDenom) | Index | Amount (duffs) | DASH |
|---|---|---|---|
| 1 (1<<0) | 0 | 1,000,010,000 | 10.0001 |
| 2 | 1 | 100,001,000 | 1.00001 |
| 4 | 2 | 10,000,100 | 0.100001 |
| 8 | 3 | 1,000,010 | 0.0100001 |
| 16 | 4 | 100,001 | 0.00100001 (smallest) |

- `GetCollateralAmount = smallest/10 = 10,000` duffs (0.0001 DASH). `GetMaxCollateralAmount = 40,000`.
- A collateral input is any amount in [10,000, 40,000].
- `GetMaxPoolAmount = 9 × 10.0001` DASH.
- `DenominationToAmount` returns 0 (uninitialized), −1 (out of bounds), −2 (more than one bit set) or −3.

Promotion and demotion constants (post-V24, `:120-259`):
- `PROMOTION_RATIO=10`, `GAP_DIVISOR=5`, `MIN_GAP_THRESHOLD=5`, `GetGapThreshold(goal)=max(goal/5, 5)`.
- `ShouldPromoteDenoms(smallCnt, largeCnt, smallFullyMixed, goal)` requires all of: `smallCnt ≥ goal/2`, `smallFullyMixed ≥ 10`, and `largerDeficit > smallerDeficit + gap`.
- `ShouldDemoteDenoms` is the mirror image and needs at least 1 fully mixed larger coin.
- `MixSideCounts::IsCovered()` means each side (session-denom inputs and session-denom outputs) is occupied by 0 or ≥ 2 participants.

##### 3.2 Network parameters

From `chainparams.cpp`:
- `nPoolMinParticipants`: 3 on mainnet (`:289`); 2 on testnet, devnet and regtest.
- `nPoolMaxParticipants`: 20 on all networks.

`COINJOIN_ENTRY_MAX_SIZE = 9`. Max pool inputs or outputs = 20 × 9 = 180 (`coinjoin.h:53-62`).

Timeouts (`coinjoin.h:44-51`):

| Constant | Value |
|---|---|
| `COINJOIN_AUTO_TIMEOUT_MIN` / `MAX` | 5 / 15 s (ticks between auto-denominate runs) |
| `COINJOIN_QUEUE_TIMEOUT` | 30 s |
| `COINJOIN_SIGNING_TIMEOUT` | 15 s |
| `COINJOIN_PENDING_OBSERVATION_TIMEOUT` | 3600 s |
| `CPendingDsaRequest::TIMEOUT` | 15 s |

V24 deployment (`DEPLOYMENT_V24`, bit 13, EHF):
- mainnet: `NEVER_ACTIVE`, window 4032
- testnet: start 1790208000, window 100
- devnet: start 1751328000
- regtest: start 0, window 250

##### 3.3 Message formats

| Cmd | Struct and serialization | Source |
|---|---|---|
| `dsa` | `CCoinJoinAccept{int32 nDenom, CMutableTransaction txCollateral, [uint8 nFlags only if stream version ≥ 70241]}`. Flags: `FLAG_PROMOTION=1`, `FLAG_DEMOTION=2`; both set is invalid | `coinjoin.h:137-178` |
| `dsq` | `CCoinJoinQueue{int32 nDenom, uint256 proTxHash, int64 nTime, bool fReady, vector vchSig}`. `masternodeOutpoint` is not serialized; it is resolved from the protx. **Hash** = `SerializeHash(SER_NETWORK)` (includes the signature); **signature hash** = `SER_GETHASH` (no signature). Signed BLS basic with the operator key | `:245-294`, `coinjoin.cpp:43-57` |
| `dsi` | `CCoinJoinEntry{vector<CTxIn> vecTxDSIn, CTransactionRef txCollateral, vector<CTxOut> vecTxOut}`. Vectors are capped at 180 on read | `:181-239` |
| `dsf` | `int32 nSessionID ‖ CTransaction finalTx` | `server.cpp:1188` |
| `dss` | `vector<CTxIn>` (signed inputs), capped at 180 | `server.cpp:254-264` |
| `dsc` | `int32 nSessionID ‖ int32 PoolMessage` | `server.cpp:1253` |
| `dssu` | `CCoinJoinStatusUpdate{int32 nSessionID, int32 PoolState, int32 PoolStatusUpdate, int32 PoolMessage}`. `nEntriesCount` is not serialized | `:113-135` |
| `dstx` | `CCoinJoinBroadcastTx{CTransactionRef tx, uint256 proTxHash, vector vchSig (not in GETHASH), int64 sigTime}`. BLS basic signature with the operator key over `SER_GETHASH` | `:298-362` |
| `senddsq` | `bool` | `net_processing.cpp:4481-4493` |

Inv types: `MSG_DSTX=16`, `MSG_DSQ=32` (DSQ invs exist from `DSQ_INV_VERSION=70234`).

Enums:
- `PoolState`: IDLE=0, QUEUE=1, ACCEPTING_ENTRIES=2, SIGNING=3, ERROR=4.
- `PoolStatusUpdate`: REJECTED=0, ACCEPTED=1.
- `PoolMessage` (int32): ERR_ALREADY_HAVE=0, ERR_DENOM=1, ERR_ENTRIES_FULL=2, ERR_EXISTING_TX=3, ERR_FEES=4, ERR_INVALID_COLLATERAL=5, ERR_INVALID_INPUT=6, ERR_INVALID_SCRIPT=7, ERR_INVALID_TX=8, ERR_MAXIMUM=9, ERR_MN_LIST=10, ERR_MODE=11, ERR_NON_STANDARD_PUBKEY=12, ERR_NOT_A_MN=13, ERR_QUEUE_FULL=14, ERR_RECENT=15, ERR_SESSION=16, ERR_MISSING_TX=17, ERR_VERSION=18, MSG_NOERR=19, MSG_SUCCESS=20, MSG_ENTRIES_ADDED=21, ERR_SIZE_MISMATCH=22. Valid range is [0, 22] (`coinjoin.h:65-91`).

**`senddsq` semantics.** After VERACK, a node that accepts transactions sends `senddsq true` (`net_processing.cpp:4220-4223`). On receipt:
- `false` → NONE
- peer version < 70234 → ALL (push full `dsq` messages)
- otherwise → INV (announce `MSG_DSQ`)

`RelayDSQ` follows this mode (`:2551-2577`).

##### 3.4 Session flow (server = masternode, `coinjoin/server.cpp`)

The server is only constructed on masternodes (`init.cpp:2239`). Masternodes have no client `CJWalletManager`; the `coinjoin start` RPC returns "not supported on masternodes" there.

1. **Client sends `dsa`** (`ProcessDSACCEPT`, `:67-127`). Rejections:
   - ERR_QUEUE_FULL if the session is already ready.
   - ERR_MN_LIST if this MN is not valid.
   - If there is no session yet: ERR_RECENT if one of our own queues is still live, or if `IsMixingThresholdExceeded(ourProTx)`.
   - Otherwise `CreateNewSession` or `AddUserToExistingSession`, followed by `dssu` ACCEPTED or REJECTED.
2. **`CreateNewSession`** (`:969-1039`). Requires state IDLE, a valid denom, a valid collateral (`IsAcceptableDSA`) and valid flags. A rebalance request needs peer version ≥ 70241 **and** V24 active at the tip (ERR_VERSION otherwise). Then:
   - `nSessionID = rand(999999)+1`, `nSessionDenom = dsa.nDenom`.
   - Latch `m_fRebalanceSession` and `m_fHasLegacyParticipant`; record the declared shape keyed by collateral hash.
   - State → QUEUE. Broadcast a `dsq` with `fReady=false`, BLS-signed.
3. **`AddUserToExistingSession`** (`:1041-1155`). Requires state QUEUE and session not ready, a valid DSA, and the same denom (ERR_DENOM otherwise).
   - Rebalance participants need v70241 and V24, and no legacy participant may be present.
   - A legacy peer cannot join a rebalance session.
   - Participants < 20.
   - No collateral may reuse a prevout of another session collateral (ERR_ALREADY_HAVE).
4. **`IsSessionReady`** (`:1158-1176`). In QUEUE: the declared sides must be covered **and** (collaterals ≥ max participants, **or** the 30 s timeout has passed and collaterals ≥ min participants). In ACCEPTING_ENTRIES it is always ready.
   - `CheckForCompleteQueue` (every 1 s): QUEUE+ready → ACCEPTING_ENTRIES, and broadcast `dsq` with `fReady=true` (`:642-661`).
5. **Client sends `dsi`** (`AddEntry`, `:711-871`). Session must be in ACCEPTING_ENTRIES. Checks:
   - entries < collaterals
   - the collateral is one of the session's, and not already used by an entry
   - size ≤ 9, or ≤ 10 in a rebalance session (ERR_MAXIMUM, collateral consumed)
   - collateral valid
   - shape not UNKNOWN (collateral consumed); in a rebalance session the shape must equal the declared shape
   - inputs not already in the pool (ERR_ALREADY_HAVE)
   - `IsValidInOuts` (below)

   The entry is then appended and `dssu` is sent to all entries.
6. **`CheckPool`** (`:302-358`):
   - All entries in and the sides covered → `CreateFinalTransaction`. All entries in but not covered → `dsc ERR_SESSION` and reset.
   - Timed out with entries ≥ min and covered → `ChargeFees`, then finalize.
   - SIGNING with all signatures present → `CommitFinalTransaction`.
7. **`CreateFinalTransaction`** (`:368-416`): concatenate entries, sort inputs and outputs BIP69, state → SIGNING, send `dsf(sessionID, tx)` to each entry's address.
8. **Client sends `dss`** (`:231-283`). Only from a participant address, in SIGNING. For each input, `AddScriptSig`: reject a duplicate scriptSig, then `VerifyScript(P2SH|STRICTENC)` against `prevPubKey` in the merged transaction.
9. **`CommitFinalTransaction`** (`:418-466`):
   - `PrioritiseTransaction(+0.1 DASH)`, then `ATMPIfSaneFee` (ATMP, with base fee ≤ 0.1 DASH).
   - Create the DSTX (BLS-signed), relay `inv MSG_DSTX`, send `dsc MSG_SUCCESS`.
   - `ChargeRandomFees`: each collateral is broadcast with probability about 10%, and the loop stops at the first one not chosen.
   - Reset.
10. **Timeouts** (`:612-635`). The state times out after 30 s (or 15 s in SIGNING) since the last step. Then `ChargeFees` and reset.
    - `ChargeFees` (`:480-538`) runs only about 34% of the time. It identifies offenders (didn't send an entry, or didn't sign) and, unless everyone or almost everyone offended, broadcasts one random offender's collateral.

**`IsValidInOuts`** (`coinjoin.cpp:264-435`, shared by server and client). The entry type is decided by counts:
- STANDARD: N:N
- PROMOTION: 10 inputs : 1 output; the output is at the larger adjacent denom
- DEMOTION: 1:10; the input is at the larger adjacent denom
- FINAL_TX: aggregate; the larger adjacent denom is allowed

Rules:
- Every output and every spent coin must have the expected denom (ERR_DENOM) and a P2PKH script (ERR_INVALID_SCRIPT).
- Scripts must be unique across all inputs and outputs (ERR_ALREADY_HAVE).
- Inputs must exist in chain+mempool; mempool parents must be InstantSend-locked (ERR_MISSING_TX).
- Σin == Σout, i.e. zero fee (ERR_FEES).
- For the final transaction, `ValidateFinalTxComposition`: `sessionIns ≥ 10·largerOuts` and `sessionOuts ≥ 10·largerIns`.
- Pre-V24, unequal counts → ERR_SIZE_MISMATCH.

**Collateral validity** (`CoinJoin::IsCollateralValid`, `coinjoin.cpp:458-503`):
- outputs non-empty, `nLockTime==0`
- outputs P2PKH or unspendable
- inputs present (mempool ones IS-locked)
- `fee ≥ 0.0001 DASH`
- `ATMPIfSaneFee(test_accept)` with fee ≤ 0.1 DASH

**DSQ spam control** (`masternode/meta.cpp:93-122`):
- `IsMixingThresholdExceeded = last_dsq ≠ 0 && last_dsq + enabledMNs/5 > nDsqCount`.
- `AllowMixing` sets `last_dsq = ++nDsqCount` and resets the mixing-tx count.
- `IsValidForMixingTxes` requires mixing-tx count ≤ 5 (`MASTERNODE_MAX_MIXING_TXES`).
- `DisallowMixing` increments the count on each accepted DSTX.

**Server-side dsq handling** (`ProcessDSQUEUE`, `server.cpp:129-199`):
- invalid denom +10; null protx and outpoint +100; unknown protx +10
- duplicate (identical, or same MN with the same readiness) → drop
- time out of ±30 s → drop
- bad BLS signature → +10
- If not ready and not over threshold: `AllowMixing`, add to the queue, relay.

##### 3.5 DSTX relay policy (`net_processing.cpp:3595-3710`, `4855-4950`)

`ValidateDSTX` steps:
1. Already known → skip.
2. `IsValidStructure` (`coinjoin.cpp:100-155`):
   - protx or outpoint is set
   - `vin.size() ≥ minParticipants`
   - all outputs denominated P2PKH
   - Pre-V24: `vin==vout ≤ 180`.
   - Post-V24: each side ≤ 200; each side ≤ 10× the other; `|in−out| % 9 == 0`.
   - A transaction that is invalid pre-V24 but valid post-V24, when V24 activates at the next block, scores PREMATURE (1). Otherwise INVALID (10).
3. Resolve the MN from protx or outpoint, searching up to 24 blocks back. Not found → misbehaving 1.
4. `IsValidForMixingTxes`; failure → silent drop.
5. BLS signature over the signature hash; failure → +10.
6. `PrioritiseTransaction(+0.1 DASH)`, `DisallowMixing`, then ATMP. On acceptance, store it in `CDSTXManager`.

Further rules:
- DSTXes bypass the recent-rejects filter.
- `CanAnnounceDstxTo` announces unbalanced or oversized DSTXes as plain `MSG_TX` to peers below 70241.
- `CDSTXManager` (`coinjoin.cpp:557-649`) tracks the confirmed height and expires an entry once it is more than 24 blocks deep, or when the tip is chainlocked after it was mined.

##### 3.6 Client (`coinjoin/client.cpp`, `walletman.cpp`)

**Scheduling** (`walletman.cpp`, `client.cpp:2439-2455`):
- `DoMaintenance` every 1 s, only when relaying transactions: `CheckQueue`, then per wallet `CheckTimeout`, `ProcessPendingDsaRequest`, and `DoAutomaticDenominating` every 5–15 ticks (random).
- `CheckPendingObservations` every 1 min, always.

**Client-side dsq** (`walletman.cpp:251-318`): the same validation as the server. A `fReady` dsq from our session's masternode → `TrySubmitDenominate`. Otherwise threshold check, `AllowMixing`, store. Clients do not relay dsq.

**`CCoinJoinClientManager::DoAutomaticDenominating`** (`:1214-1262`):
- Prunes the "used masternodes" set when it exceeds 0.9 × enabled MNs.
- Adds sessions up to `-coinjoinsessions`.
- For each session: `CheckAutomaticBackup` (`:894-960`, legacy wallets only; keys-left < 50 stops mixing, < 100 triggers an auto-backup), and `WaitForAnotherBlock` (non-multisession: 1 block after the last success).

**Per-session `DoAutomaticDenominating`** (`:962-1212`):
- Requires synced, unlocked-for-mixing, no entries, enabled MNs > 0 (not required on regtest).
- `needsAnonymized = coinjoinamount·COIN − anonymizedBalance`. Post-V24 it also looks for a promote/demote opportunity across adjacent denom pairs.
- Creates denominations if `nonDenomAnonymizable ≥ smallest + collateral` and there is still balance to denominate (`CreateDenominated`).
- Creates collateral outputs if missing (`MakeCollateralAmounts`).
- Non-multisession: waits if there are unconfirmed denominated outputs.
- Creates or re-validates `txMyCollateral` and locks its input.
- Rebalance attempts come first (`JoinExistingQueue` / `StartNewQueue` with target denom and flag), then standard `JoinExistingQueue` and `StartNewQueue`.

**`JoinExistingQueue`** (`:1384-1471`):
- Iterates untried, non-timed-out queues (denom-filtered for rebalance).
- Skips MNs that are about to be paid: `nLastPaidHeight + weightedCount < height + WinnersToSkip` (8, or 1 on devnet/regtest).
- Needs matching denominated inputs; marks the MN used; skips if already connected as a masternode connection.
- Sets `pendingDsaRequest` and `AddPendingMasternode`, state QUEUE.

**`StartNewQueue`** (`:1473-1644`): up to 10 random unused MNs, with the same skip and threshold checks. The denom is chosen randomly among the wallet's ready-to-mix amounts.

**`ProcessPendingDsaRequest`** (`:1646-1696`): once connected, sends `dsa`. A rebalance request to an MN below 70241 aborts. The request expires after 15 s.

**Status and state handling**:
- `dssu` (`:414-462`): REJECTED → ERROR plus unlock. ACCEPTED in QUEUE with a nonzero session ID → adopt the session ID.
- A ready `dsq` → `SubmitDenominate` (`:1740-1812`):
  - Standard: select up to the max pool amount of session-denom coins; try each rounds bucket `0..rounds+3`; pick the bucket with the most inputs (ties → lower rounds), else `0..rounds−1`.
  - `PrepareDenominate` takes ≤ 9 pairs and randomly skips about 1/5 after the first; each output uses a fresh reserved key.
  - Promotion: 10 inputs → 1 larger output. Demotion: 1 input → 10 outputs (`:1896-2020`).
  - Then `dsi`.
- `dsf` → **`SignFinalTransaction`** (`:464-632`):
  - Re-sort BIP69; the hash must be unchanged.
  - `IsValidInOuts(final=true)`.
  - All my outputs must be present and all my inputs found.
  - If rebalance is possible: each side containing my session-denom coins must also contain someone else's.
  - Sign with `SIGHASH_ALL|SIGHASH_ANYONECANPAY`.
  - Send `dss` with only my inputs. State SIGNING.
- `dsc` (`:634-674`): on success, keep the keys, move the mixed inputs to **pending observations** (kept locked and persisted under `cj_pending_obs` until spent, or released after 1 h if not spent in chain or mempool, `:681-869`), unlock the rest, reset.

**Client timeouts** (`:304-336`): QUEUE 30+10 s, SIGNING 15+10 s → ERROR, unlock, return keys. ERROR → IDLE after 10 s.

**Collateral and denomination creation:**
- `CreateCollateralTransaction` (`:2149-2192`): one random collateral-sized UTXO. If its value ≥ 0.0002, change goes to a new key with `value − 0.0001`; otherwise the single output is `OP_RETURN` with value 0.
- `MakeCollateralAmounts` (`:2022-2147`): per address tally; outputs are [max collateral + remainder], or 2 equal collaterals, or 1.
- `CreateDenominated` (`:2194-2412`): fills each denom up to `denomsgoal`, then to `denomshardcap` (the largest denom is uncapped), ≤ 500 outputs per transaction (`COINJOIN_DENOM_OUTPUTS_THRESHOLD`), optionally plus one max-collateral output.
- `CTransactionBuilder` (`util.cpp:120-170`): fee rate = max(required, paytxfee); change goes back to the source address unless `avoid_reuse` is set; no extra inputs.

##### 3.7 Options

From `options.h`, `options.cpp:61-73` and `wallet/init.cpp:110-117`. Values are clamped to [min, max]; `-coinjoindenomshardcap < -coinjoindenomsgoal` is an init error (`wallet/init.cpp:171`).

| Arg | Default | Range |
|---|---|---|
| `-enablecoinjoin` | 0 | bool |
| `-coinjoinautostart` | false | bool |
| `-coinjoinmultisession` | false | bool |
| `-coinjoinsessions` | 4 | 1–10 |
| `-coinjoinrounds` | 4 | 2–16 (plus `COINJOIN_RANDOM_ROUNDS`=3) |
| `-coinjoinamount` | 1000 DASH | 2–MAX_MONEY/COIN |
| `-coinjoindenomsgoal` | 50 | 10–100000 |
| `-coinjoindenomshardcap` | 300 | 10–100000 |

Runtime RPCs `setcoinjoinrounds` and `setcoinjoinamount` live in `wallet/rpc/wallet.cpp:58-120`.

##### 3.8 Wallet integration (`wallet/coinjoin.cpp`)

**`GetRealOutpointCoinJoinRounds`** (`:261-370`, memoized in `mapOutpointRoundsCache`). Return values:
- −1 not in wallet (not memoized)
- −4 bad output index
- −3 collateral amount
- −2 not denominated
- 0 if any output of the transaction is non-denominated, or the transaction's debit ≠ credit (a fee was paid), or (post-V24) one of my inputs has a different denom (promotion/demotion output), or no denominated parent was found
- otherwise `1 + min(rounds of my inputs)`, capped at `nRoundsMax = MAX_COINJOIN_ROUNDS(16) + randomRounds(3) = 19`; recursion depth is also bounded by this

**`IsFullyMixed`** (`:405-426`):
- `rounds < R` → false.
- If `rounds < R+3`: `h = SHA256(ser(COutPoint) ‖ salt)` (single SHA256); if `LE64(h) % 2 == 0` → false. This gives the exponential decay 50/25/12.5%.
- Otherwise true.

**CoinType** (`wallet/coinselection.h:25-36`, filter at `spend.cpp:252-283`):

| CoinType | Value | Selects |
|---|---|---|
| ALL_COINS | 0 | everything |
| ONLY_FULLY_MIXED | 1 | denominated and fully mixed; no change allowed, any excess becomes fee (`spend.cpp:991-993`) |
| ONLY_READY_TO_MIX | 2 | denominated and not fully mixed |
| ONLY_NONDENOMINATED | 3 | not collateral-sized and not denominated |
| ONLY_MASTERNODE_COLLATERAL | 4 | 1000 or 4000 DASH outputs, including locked ones |
| ONLY_COINJOIN_COLLATERAL | 5 | collateral-sized outputs |

`use_cj` in the send RPCs → `ONLY_FULLY_MIXED`; the transaction is tagged with `mapValue {"DS":"1"}`.

**Wallet DB keys** (`walletdb.cpp:30-80, 224-252, 654-668`):

| Key | Value |
|---|---|
| `"cj_salt"` | uint256 |
| `"ps_salt"` | uint256, legacy; migrated to `cj_salt` on read |
| `"cj_pending_obs"` | `map<COutPoint, int64>` |
| `("g_object", hash)` | `Governance::Object`; the hash is checked on load |

Pending observations also use persistent `"lockedutxo"` entries. If no salt exists when the wallet loads, a random one is generated (`wallet.cpp:2541`).

##### 3.9 CoinJoin RPCs (`rpc/coinjoin.cpp`)

| RPC | Behavior |
|---|---|
| `coinjoin start` | Errors on a masternode, when the wallet is locked (mixing unlock is allowed), or when already mixing. Requires `-enablecoinjoin` |
| `coinjoin stop` | Errors if not mixing |
| `coinjoin reset` | `resetPool` |
| `coinjoin status` | Session status strings |
| `coinjoinsalt generate [overwrite=false]` | Refuses if a salt exists, the wallet is mixing, the wallet has a CoinJoin balance (anonymized + denominated), or private keys are disabled — unless `overwrite` |
| `coinjoinsalt get` | Errors if the salt is null |
| `coinjoinsalt set salt [overwrite]` | Rejects a zero salt; same guards. Calls `ClearCoinJoinRoundsCache` |
| `getcoinjoininfo` | On a masternode: server `{queue_size, denomination, state, entries_count}`. Otherwise: options (enabled, multisession, max_sessions, max_rounds, max_amount, denoms_goal, denoms_hardcap), queue_size, running, pending_inputs, sessions[], keys_left, warnings |

---

#### 4. Consensus-critical vs policy

| Area | Consensus | Policy or local |
|---|---|---|
| Superblock height rule, `GetPaymentsLimit` maths, V20 10%→20% treasury, coinbase ≤ reward + limit, `bad-sb-start` hash | Yes | |
| Exact payee and amount match of the chosen trigger (`IsValidSuperblock`), V24 duplicate-match rule | Yes, but depends on local governance state; skipped when chainlocked or not synced | |
| Trigger selection (max absYes FUNDING, funding threshold W/10, Evo weight 4) | Effectively consensus for synced nodes | |
| Proposal validation, collateral, vote rate limits, orphans, sync, governance.dat | | Policy |
| CoinJoin denominations, DSTX structure, signatures, collateral | Not consensus; mixing transactions are ordinary transactions | P2P and policy (misbehaving scores, relay) |

---

#### 5. Notes for a Rust reimplementation

1. **Hashing needs care.** Object and vote hashes are not their serialization:
   - The object hash serializes the data as a hex **string**, appends `uint8 0, uint32 0xffffffff` after the outpoint, includes `vchSig`, and excludes `collateralHash`.
   - The vote hash orders signal before outcome; the vote serialization orders outcome before signal.
   - DSQ: `GetHash` includes the signature (`SER_NETWORK`); the signature hash excludes it.
   - Build golden-vector tests against Dash Core for each of these.
2. **Vote signature selection** must reproduce:
   - PROPOSAL+FUNDING → ECDSA voting key.
   - Everything else → BLS operator key, basic scheme, `VerifyInsecure(…, false)`.
   - Mainnet message string format, `"DarkCoin Signed Message:\n"` magic, compact-recovery verification. Testnet signs the raw signature hash.
   - Strict 65- or 96-byte signature length on network reads.
3. **The trigger JSON template must be byte-exact.** Masternodes converge by data hash, so the formatter needs exact spacing, `|` separators, 8-decimal amounts, and `"type":2`.
4. **Superblock validation is state-dependent.** A Rust full node must keep the governance store (triggers plus votes) in sync to validate superblocks at the tip. Mirror Dash's escape hatches: skip when chainlocked or not masternode-synced, and always enforce the max-value bound. Don't punish peers for `bad-cb-amount`.
5. **Treat `std::map` ordering as behavior.** `GetBestSuperblock` tie-break order (map iteration by hash, strict `>`) and the approved-proposal sort (absYes descending, then `UintToArith256(hash)` descending) are observable. Use the same comparators: uint256 map ordering is a byte-wise memcmp of the little-endian storage, while arith comparison is numeric.
6. **Collateral checks need a txindex** for proposals (6 confirmations; 1–5 → postponed). Plan a txindex or an equivalent lookup.
7. **Persistence.** The `governance.dat` layout (section 2.13) embeds a full `CDeterministicMNList`. Compatibility with Core's file is optional: a version-string mismatch already causes a silent rebuild from the network. Implementing your own format is reasonable, but the network sync semantics (govsync bloom, fulfilled-request limits, ssc counts) must match to avoid bans.
8. **Plan CoinJoin V24 into the design.** Rebalance sessions are version-gated on the `dsa` wire field (`nFlags` present only at ≥ 70241). Unbalanced DSTX structure rules and the "PREMATURE" penalty tolerance one block around activation also need to be implemented. Pre-V24 rules still apply on mainnet, where V24 is currently `NEVER_ACTIVE`.
9. **CoinJoin server** (masternode parity): session state machine, BIP69 sort of the final transaction, zero-fee `IsValidInOuts`, collateral ATMP with a 0.1 DASH sane-fee cap, priority delta +0.1 DASH for final and DSTX transactions, probabilistic fee charging, DSQ threshold `enabledMNs/5` against the global `nDsqCount`, and the ≤ 5 DSTX per MN counter.
10. **Wallet parity.** Rounds algorithm (memoized; −10/−1/−2/−3/−4 sentinels; the V24 rule that resets a converted coin to 0 rounds), salt-based `IsFullyMixed`, `ONLY_FULLY_MIXED` with no change output, `cj_salt`/`ps_salt`/`cj_pending_obs`/`g_object` DB keys, and pending-observation locks that survive restart.

### A.6 Wallet, RPC server, REST, ZMQ, CLI tools, init/config (all options), stats, notifications, Qt

*Section numbers below (§1, §2, …) are local to A.6.*

Source tree: `/work/home/agent/workspace/dash` (branch `develop`, HEAD `15db478750`). All paths below are relative to that root. Nothing was modified.

**Findings most likely to affect the Rust spec:**
- New wallets are descriptor wallets by default. They use SQLite and are created from a BIP39 mnemonic.
- Legacy wallets are BDB 4.8 files whose HD state lives in a `hdchain` / `chdchain` record (seed, mnemonic, passphrase, per-account counters).
- Mnemonic validation has a checksum bug, and the seed salt is not NFKD-normalised. An importer must copy both quirks or some existing wallets will not import.
- Coin selection only ever runs Knapsack in practice; BnB and SRD are given an empty input list.
- Whether a CoinJoin output counts as "fully mixed" depends on the wallet's `cj_salt` record, so that record must be imported.
- There is no JSON-RPC 2.0 support. Replies always carry `result`, `error` and `id`.
- `-deprecated-platform-user` is accepted but does nothing.
- `-txindex` defaults to 1 in Dash (0 upstream).

---

#### 1. Wallet (`src/wallet/`)

##### 1.1 Legacy vs descriptor wallets, and the defaults
- **`createwallet` RPC** (`wallet/rpc/wallet.cpp:602-700`) defaults to `descriptors=true` (line 613).
  - Arguments in order: `wallet_name`, `disable_private_keys`=false, `blank`=false, `passphrase`, `avoid_reuse`=false, `descriptors`=true, `load_on_startup`, `external_signer`=false.
  - Dash v21 inserted `descriptors` at position 5. If you pass `descriptors` positionally without `load_on_startup`, the call fails with RPC_INVALID_PARAMETER (lines 655-667).
  - A descriptor wallet requires SQLite support in the build; a legacy wallet requires BDB.
- **`dash-wallet create`** (`wallet/wallettool.cpp:152-176`) also defaults to descriptor/SQLite unless `-legacy` is given. Passing both `-legacy` and `-descriptors` is an error.
- **First run of a new wallet** (`wallet.cpp:3253-3330`): `SetMinVersion(FEATURE_LATEST)`, then:
  - Legacy wallet with `-usehd` (default 1): a new HD chain is built from `-hdseed` (hex; takes priority over `-mnemonic`, with a warning, `wallet/init.cpp:165`) or else from `-mnemonic` / `-mnemonicpassphrase` (random if empty). Minimum version becomes `FEATURE_HD`.
  - Descriptor wallet: `SetupDescriptorScriptPubKeyMans(mnemonic, passphrase)` using `-mnemonic` / `-mnemonicpassphrase`. If the mnemonic is empty, `CMnemonic::Generate(-mnemonicbits)` is used (default 128 bits = 12 words; allowed 128/160/192/224/256).
  - An automatic backup is taken right after creation.
- **Default wallet loading** (`wallet/load.cpp:57-80`): if `-wallet` is not set and an unnamed top-level wallet exists in walletdir, wallet `""` is loaded. Wallets marked load-on-startup are listed in `settings.json` under the key `"wallet"`.
- **Wallet location** (`db.cpp:74-80`): a wallet is a directory under walletdir containing `wallet.dat`. A bare top-level BDB file is also accepted.

##### 1.2 Legacy HD (Dash-specific, not Bitcoin's)
- **`CHDChain`** (`hdchain.h:24-118`) is serialised in this order:
  - `int nVersion` (=1)
  - `uint256 id` (= `Hash(seed)`, i.e. SHA256d, `hdchain.cpp:128-131`)
  - `bool fCrypted`
  - `vector<uchar> vchSeed` (64-byte BIP39 seed)
  - `vector<uchar> vchMnemonic`
  - `vector<uchar> vchMnemonicPassphrase`
  - `map<uint32, CHDAccount> mapAccounts`
- **`CHDAccount`**: `uint32 nExternalChainCounter`, `uint32 nInternalChainCounter`.
- **`CHDPubKey`** (`hdchain.h:120-139`): `int nVersion`(=1), `CExtPubKey extPubKey` (BIP32 74-byte encoding), `uint256 hdchainID`, `uint32 nAccountIndex`, `uint32 nChangeIndex`.
- **Derivation path** (`hdchain.cpp:142-178`): `m/44'/coin'/account'/change/index`. `coin` comes from `nExtCoinType`: mainnet 5 (`chainparams.cpp:267`); testnet, devnet and regtest 1 (lines 462, 636, 910). Stock wallets only use account 0.
- **Private keys are not stored for HD keys.** They are re-derived on demand; only `hdpubkey` and `keymeta` records are written (`scriptpubkeyman.cpp:1323-1387`).
- **BIP39** (`bip39.cpp`):
  - Seed = `PBKDF2-HMAC-SHA512(mnemonic, "mnemonic"+passphrase, 2048 iterations, 64 bytes)`. The salt is truncated to 256 bytes and there is **no NFKD normalisation** (`bip39.cpp:141-149`). The passphrase must be 256 characters or fewer (`hdchain.cpp:60`).
  - Checksum bug: `Check()` computes the mask as `(2 ^ checksum_length)` using XOR rather than a power (`bip39.cpp:134-137`). Validation is therefore weaker than BIP39. Words must come from the English list and the word count must be 12-24 and divisible by 3.
- **Encrypted HD chain** (`scriptpubkeyman.cpp:520-560`): seed, mnemonic and passphrase are each `EncryptSecret(masterKey, plaintext, IV = first 16 bytes of chain.id)`. The resulting ciphertexts are stored in the same `CHDChain` fields, with `fCrypted=1`, under the key `chdchain`.

##### 1.3 Descriptor wallets
- **Supported descriptor functions** (`script/descriptor.cpp`): `pk`, `pkh`, `sh`, `multi`, `sortedmulti`, `combo`, `addr`, `raw`. There is no `wpkh`, `wsh` or `tr`. `OutputType` has only `LEGACY` (`outputtype.h:17`).
- **Default descriptors** (`wallet.cpp:4379-4402`, `scriptpubkeyman.cpp:2510-2564`). `xpub` is the master key, and the stored private key is the master key:
  - BIP44 external (active): `pkh(xpub/44h/<coin>h/0h/0/*)`
  - BIP44 internal (active change): `pkh(xpub/44h/<coin>h/0h/1/*)`
  - DIP9 CoinJoin (not active): `pkh(xpub/9h/<coin>h/4h/0h/0/*)`
- **Descriptor ID** = single SHA256 of `desc.ToString()`, which includes the `#checksum` (`descriptor.cpp:1271-1277`).
- **`WalletDescriptor`** is serialised as `string desc`, `uint64 creation_time`, `int32 next_index`, `int32 range_start`, `int32 range_end` (`walletutil.h`).
- **Mnemonics are stored with the descriptor key.** This is a Dash extension, shown under `walletdescriptorkey` in 1.4.
- **Migration** (`migratewallet`, EXPERIMENTAL, `wallet/rpc/wallet.cpp:1111`; `LegacyScriptPubKeyMan::MigrateToDescriptor`, `scriptpubkeyman.cpp:1880+`):
  - Each loose (non-HD) key becomes `combo(origin pubkey)`.
  - The HD chain becomes inactive `combo()` descriptors for the external and internal chains.
  - The active `pkh()` descriptors are then created with `next_index` advanced to the legacy counters (`MigrationData` in `walletutil.h`).
  - A legacy wallet with more than one account is rejected.
  - Records in `DBKeys::LEGACY_TYPES` are erased.

##### 1.4 Wallet database records (`walletdb.cpp:37-77`)
Every key starts with a compact-size string type, followed by the fields listed.

| Type string | Key after type | Value |
|---|---|---|
| `acentry` | legacy | ignored |
| `activeexternalspk` | — | uint256 descriptor id (output type is implicitly LEGACY) |
| `activeinternalspk` | — | uint256 descriptor id |
| `bestblock` | — | CBlockLocator (an empty locator is always written) |
| `bestblock_nomerkle` | — | CBlockLocator (int version + vector<uint256>) |
| `ckey` | CPubKey | vector<uchar> encrypted privkey, uint256 SHA256d(ciphertext) checksum (optional on read) |
| `chdchain` | — | CHDChain with fCrypted=1 |
| `cj_pending_obs` | — | map<COutPoint, int64> |
| `cj_salt` | — | uint256 CoinJoin salt |
| `cscript` | uint160 script hash | CScript |
| `defaultkey` | — | CPubKey (validated only) |
| `destdata` | string address, string key | string. Key `"used"` = avoid-reuse marker; key `"rr<id>"` = serialised receive request |
| `flags` | — | uint64 wallet flags |
| `g_object` | uint256 hash | Governance::Object: hashParent, revision, time, collateralHash, vchData, type, masternodeOutpoint, vchSig (`governance/common.h:70`) |
| `hdchain` | — | CHDChain, plaintext |
| `hdpubkey` | CPubKey | CHDPubKey |
| `key` | CPubKey | CPrivKey (DER), uint256 SHA256d(pubkey‖privkey) (optional) |
| `keymeta` | CPubKey | CKeyMetadata: int nVersion, int64 nCreateTime, and when nVersion ≥ 12, KeyOriginInfo + bool has_key_origin |
| `lockedutxo` | uint256 txid, uint32 n | uint8 `'1'` |
| `mkey` | uint32 id | CMasterKey |
| `minversion` | — | int |
| `name` | string address | string label |
| `orderposnext` | — | int64 |
| `platform_data` | string key (e.g. `"platform/identity/0"`) | vector<uchar> |
| `pool` | int64 index | CKeyPool: int version, int64 nTime, CPubKey, bool fInternal (optional) |
| `purpose` | string address | string purpose |
| `ps_salt` | — | uint256 legacy PrivateSend salt; read for migration to `cj_salt` |
| `settings` | — | ignored |
| `tx` | uint256 txid | CWalletTx, see below |
| `version` | — | int CLIENT_VERSION of the last client that opened the wallet |
| `walletdescriptor` | uint256 id | WalletDescriptor |
| `walletdescriptorcache` | uint256 id, uint32 key_exp_index [, uint32 der_index] | vector<uchar> 74-byte xpub (parent if no der_index, otherwise derived) |
| `walletdescriptorlhcache` | uint256 id, uint32 key_exp_index | 74-byte xpub (last hardened) |
| `walletdescriptorkey` | uint256 id, CPubKey | pair(CPrivKey, uint256 SHA256d(pub‖priv)), then optional SecureString mnemonic, SecureString passphrase (Dash extension, lines 285-293, 741-790) |
| `walletdescriptorckey` | uint256 id, CPubKey | vector<uchar> secret, then optional vector<uchar> encrypted mnemonic, encrypted passphrase (IV = pubkey.GetHash()) |
| `watchmeta` | CScript | CKeyMetadata |
| `watchs` | CScript | uint8 `'1'` |
| `wkey` | — | unsupported; load fails |

- **`CWalletTx`** (`transaction.h:239-280`) is serialised as: tx, hashBlock, vector (empty), int nIndex, vector (empty), mapValue (keys `fromaccount`, `n`=orderpos, `timesmart`), vOrderForm, `fTimeReceivedIsTxTime`, `nTimeReceived`, `fFromMe`, bool dummy.
- **Legacy-only types** (`LEGACY_TYPES`): `ckey`, `chdchain`, `cscript`, `defaultkey`, `hdchain`, `hdpubkey`, `keymeta`, `key`, `wkey`, `pool`, `ps_salt`, `watchmeta`, `watchs`. If any appears in a descriptor wallet, loading fails with UNEXPECTED_LEGACY_ENTRY.

##### 1.5 Wallet versions and flags (`walletutil.h`)
- **Features:** `FEATURE_BASE`=10500, `FEATURE_WALLETCRYPT`=40000, `FEATURE_COMPRPUBKEY`=60000, `FEATURE_HD`=120200, `FEATURE_LATEST`=`FEATURE_HD`. A `minversion` above `FEATURE_LATEST` gives TOO_NEW.
- **Flags:**

| Flag | Bit |
|---|---|
| `AVOID_REUSE` | 1<<0 |
| `KEY_ORIGIN_METADATA` | 1<<1 |
| `LAST_HARDENED_XPUB_CACHED` | 1<<2 |
| `DISABLE_PRIVATE_KEYS` | 1<<32 |
| `BLANK_WALLET` | 1<<33 |
| `DESCRIPTORS` | 1<<34 |
| `EXTERNAL_SIGNER` | 1<<35 |

  An unknown flag at bit 32 or above makes the wallet refuse to open.

##### 1.6 Encryption (`crypter.h`, `crypter.cpp`, `wallet.cpp:800-830`)
- **`CMasterKey`**: `vchCryptedKey`, `vchSalt` (8 bytes), `uint nDerivationMethod` (0 = EVP_BytesToKey-style SHA512), `uint nDeriveIterations`, `vchOtherDerivationParameters`.
- **Passphrase key derivation**: `BytesToKeySHA512AES(salt, passphrase, rounds)` yields a 32-byte key and 16-byte IV for AES-256-CBC with PKCS padding. That key encrypts a random 32-byte master key.
- **Iteration calibration**: aims for 100 ms, minimum 25000.
- **Key encryption**: each private key is `AES256CBC(masterKey, IV = first 16 bytes of SHA256d(pubkey))`.

##### 1.7 Storage formats
- **BDB 4.8.30** (`depends/packages/bdb.mk`): btree, logical database name `"main"` (`bdb.cpp:360`), log directory `database/`, error file `db.log`. File magic `0x00053162` (either byte order) at offset 12 (`db.cpp:83-105`).
- **SQLite** (`sqlite.cpp`):
  - Schema: `CREATE TABLE main(key BLOB PRIMARY KEY NOT NULL, value BLOB NOT NULL)`.
  - `PRAGMA application_id` = int32 of the big-endian network magic:

    | Network | Magic | application_id |
    |---|---|---|
    | main | `bf0c6bbd` | -1089705027 |
    | test | `cee2caff` | -823997697 |
    | devnet | `e2caffce` | -490012722 |
    | regtest | `fcc1b7dc` | -54413348 |

  - `user_version` = 0. Other pragmas: `locking_mode=exclusive`, `fullfsync=true`. Detection reads the `"SQLite format 3\0"` header and checks the application id (`db.cpp:108-140`).
- **Read/write support**: both formats are read and written when compiled in. There is no BDB-free read-only parser.

##### 1.8 `dumpwallet` (legacy only; `wallet/rpc/backup.cpp:922-1100`)
- Refuses to overwrite an existing file.
- Header lines: `# Wallet dump created by Dash Core <ver>`, `# * Created on <ISO8601>`, `# * Best block at time of backup was <h> (<hash>),`, `#   mined on <ISO8601>`.
- If an HD chain exists: `# mnemonic: …`, `# mnemonic passphrase: …`, `# HD seed: <hex>`, `# extended private masterkey: xprv…`, `# extended public masterkey: xpub…`, then per account `# external chain counter: N` and `# internal chain counter: N`.
- Key lines: `<WIF> <ISO8601> label=<enc>|reserve=1|change=1 # addr=<addr>[ hdkeypath=m/44'/5'/0'/0/i]`.
- Script lines: `<hex> <time> script=1 # addr=…`.
- Ends with `# End of dump`.
- `dumphdinfo` (legacy only) returns `hdseed`, `mnemonic`, `mnemonicpassphrase`. For descriptor wallets, `listdescriptors private=true` returns `mnemonic` / `mnemonicpassphrase` (backup.cpp:1982, 2079).

##### 1.9 Coin selection and transaction building (`spend.cpp`, `coinselection.h`)
- **`CoinType`** values: `ALL_COINS`, `ONLY_FULLY_MIXED`, `ONLY_READY_TO_MIX`, `ONLY_NONDENOMINATED` (excludes collateral amounts), `ONLY_MASTERNODE_COLLATERAL` (includes locked outputs), `ONLY_COINJOIN_COLLATERAL`. Filters are at spend.cpp:252-283.
- **Only Knapsack is effective**: `positive_groups.clear()` at line 543 gives BnB and SRD an empty list. Knapsack receives an `ONLY_FULLY_MIXED` flag and `m_default_max_tx_fee`. The least-waste result is chosen.
- **Fully mixed spends have no change**; any excess goes to the fee (lines 988-993).
- **Error messages**: "Unable to locate enough non-denominated funds…" and "Unable to locate enough mixed funds…".
- **"Fully mixed"** (`wallet/coinjoin.cpp:405-426`): rounds ≥ `-coinjoinrounds`. If rounds are below rounds + random rounds, the output re-mixes when `SHA256(outpoint‖cj_salt)` LE64 is even.
- **Fees** (`wallet/fees.cpp`): standard Bitcoin order — coin control, then `paytxfee`, then `estimatesmartfee`, then `fallbackfee`, floored by mempool minimum and required fee. There is no RBF and no `bumpfee`. Fee rates are per kB.
- **InstantSend and ChainLocks**:
  - `CachedTxIsTrusted` treats an IS-locked transaction as trusted at depth 0 (`receive.cpp:263`).
  - `IsTxChainLocked` caches the result; once a transaction is chainlocked, the IS flag is cleared (`wallet.cpp:4100-4123`).
  - Balance RPCs take `addlocked` (coins.cpp:86/127/215).
  - Transaction RPCs report `instantlock` (IS or CL), `instantlock_internal`, `chainlock` (transactions.cpp:29-31).
  - `Balance` adds `m_anonymized`, `m_denominated_trusted`, `m_denominated_untrusted_pending` (`receive.h:50-62`).

##### 1.10 Keypool, backups and wallet notifications
- `-keypool` default 1000.
- Auto-backups go to `<datadir/net>/backups` or `-walletbackupsdir`. File name: `<walletname>.YYYY-MM-DD-HH-MM`. The number kept is `-createwalletbackups` (default 10); `keys_left` is reported in `getwalletinfo`.
- Masternode collaterals are auto-locked at startup.
- `-dustprotectionthreshold` auto-locks dust outputs.

##### 1.11 Wallet RPCs (`wallet/rpc/wallet.cpp:1255-1328`, plus Dash wallet-gated tables)
- **`rawtransactions`**: `fundrawtransaction`.
- **`wallet`**:
  - Wallet lifecycle: `abandontransaction`, `abortrescan`, `backupwallet`, `createwallet`, `restorewallet`, `encryptwallet`, `loadwallet`, `unloadwallet`, `listwalletdir`, `listwallets`, `migratewallet`, `upgradewallet`, `upgradetohd`, `wipewallettxes`.
  - Addresses and labels: `addmultisigaddress`, `getaddressesbylabel`, `getaddressinfo`, `getnewaddress`, `getrawchangeaddress`, `listaddressbalances`, `listaddressgroupings`, `listlabels`, `setlabel`.
  - Balances and transactions: `getbalance`, `getbalances`, `getunconfirmedbalance`, `getreceivedbyaddress`, `getreceivedbylabel`, `gettransaction`, `getwalletinfo`, `listreceivedbyaddress`, `listreceivedbylabel`, `listsinceblock`, `listtransactions`, `listunspent`, `rescanblockchain`.
  - Keys and import/export: `dumphdinfo`, `dumpprivkey`, `dumpwallet`, `importaddress`, `importelectrumwallet`, `importdescriptors`, `importmulti`, `importprivkey`, `importprunedfunds`, `importpubkey`, `importwallet`, `keypoolrefill`, `listdescriptors`, `newkeypool`, `removeprunedfunds`, `sethdseed`.
  - Spending and signing: `lockunspent`, `listlockunspent`, `send`, `sendmany`, `sendtoaddress`, `sendall`, `settxfee`, `signmessage`, `signrawtransactionwithwallet`, `simulaterawtransaction`, `walletprocesspsbt`, `walletcreatefundedpsbt`.
  - Settings and locking: `setcoinjoinrounds`, `setcoinjoinamount`, `setwalletflag`, `walletlock`, `walletpassphrasechange`, `walletpassphrase`.
  - External signer builds only: `walletdisplayaddress`.
- **Signer**: `enumeratesigners`.
- **Dash wallet-gated (`#ifdef ENABLE_WALLET`)**:
  - `gobject prepare`, `gobject list-prepared`, `gobject vote-many`, `gobject vote-alias` (governance.cpp:1020-1031).
  - `coinjoin start|stop|reset|status`, `coinjoinsalt get|set|generate` (coinjoin.cpp).
  - `protx register*`, `update_*`, `revoke`, `shared*` (evo.cpp:2433-2463).
  - `masternode outputs` (masternode.cpp:814).
- **`upgradetohd`** arguments: `mnemonic`="", `mnemonicpassphrase`="", `walletpassphrase`="", `rescan` (default false when the mnemonic is empty).
- **`sethdseed`**: legacy only.

##### 1.12 PSBT and external signer
- **PSBT** (`psbt.h:21-45`): magic `psbt\xff`.
  - Global fields: UNSIGNED_TX 0x00, XPUB 0x01, VERSION 0xFB, PROPRIETARY 0xFC.
  - Input fields: NON_WITNESS_UTXO 0x00, PARTIAL_SIG 0x02, SIGHASH 0x03, REDEEMSCRIPT 0x04, BIP32_DERIVATION 0x06, SCRIPTSIG 0x07, RIPEMD160 0x0A, SHA256 0x0B, HASH160 0x0C, HASH256 0x0D, PROPRIETARY 0xFC.
  - Output fields: REDEEMSCRIPT 0x00, BIP32_DERIVATION 0x02, PROPRIETARY 0xFC.
  - No witness or taproot fields.
- **External signer**: `-signer=<cmd>` (HWI protocol). On wallet creation it calls `getdescriptors` and reads the `receive` / `internal` arrays (`wallet.cpp:4423-4450`).

##### 1.13 `dash-wallet` tool (`bitcoin-wallet.cpp:28-48`, `wallettool.cpp`)
- **Commands**: `info`, `create`, `salvage` (experimental), `wipetxes`, `dump`, `createfromdump`.
- **`info` output**: Name, Format, Descriptors, Encrypted, HD, Keypool Size, Transactions, Address Book.
- **Dump file format** (`dump.cpp`):
  - First line `BITCOIN_CORE_WALLET_DUMP,1`, then `format,<bdb|sqlite>`.
  - Then `hex(key),hex(value)` lines.
  - Last line `checksum,<hex SHA256d of all prior lines>`.

---

#### 2. RPC server

- **HTTP JSON-RPC** (`httprpc.cpp`):
  - Only POST (otherwise 405). Basic auth; on failure, 401 after a 250 ms sleep with `WWW-Authenticate: Basic realm="jsonrpc"`.
  - Handler registered at `/`, and at `/wallet/` (prefix match) when wallet support exists. The wallet name is URL-decoded from the URI.
  - Batch requests (arrays) are handled by `JSONRPCExecBatch` (server.cpp:399).
  - Errors map to HTTP status: -32600 → 400, -32601 → 404, others → 500.
- **No JSON-RPC 2.0.** `JSONRPCReplyObj` always returns `{result, error, id}` (`rpc/request.cpp:40-50`). `parse` requires `method` and accepts `params` as an array, an object, or absent.
- **Auth** (`httprpc.cpp:313-340`, `rpc/request.cpp`):
  - If `rpcpassword` is empty, a cookie is generated: `__cookie__:<64 hex>` written to `-rpccookiefile` (default `.cookie` in the network datadir), via a `.tmp` file and rename. It is deleted on shutdown.
  - Otherwise `rpcuser:rpcpassword`.
  - `-rpcauth=user:salt$hash`, where hash = `HMAC-SHA256(key=salt, msg=password)` in hex.
- **Whitelist**:
  - `-rpcwhitelist=user:m1,m2` (split on `,` or space). Repeated entries for a user are intersected. `-rpcwhitelistdefault` defaults to true if any whitelist is set.
  - Composite Dash commands are matched as method name concatenated with the first parameter, with no space, e.g. `quorumlist` (`httprpc.cpp:111-119`; `test/functional/rpc_whitelist.py:61`).
  - A refused call returns 403.
- **External users** (`httpserver.cpp:336-353, 506-509`): users listed in `-rpcexternaluser` get a separate work queue of depth `-rpcexternalworkqueue`.
- **Platform user**: `-deprecated-platform-user` is registered (`init.cpp:835`) but is never read; it is a no-op. `-platform-user` was removed in v23.
- **Bind and allow** (`httpserver.cpp:238-420`):
  - 127.0.0.0/8 and ::1 are always allowed.
  - Binding defaults to `::1` and `127.0.0.1`. `-rpcbind` is honoured only together with `-rpcallowip`. `-rpcallowip` without `-rpcbind` binds `0.0.0.0` and `::`.
  - Maximum header size 8192 bytes; maximum body size `MAX_SIZE`.
- **Dispatch** (`server.cpp:496-526`):
  - In warmup, every call returns -28.
  - Sub-commands are registered as `"method sub"` (e.g. `protx register`). If `params[0]` is a string matching a sub-command, the request is squashed.
  - Named arguments: aliases are separated with `|`. An `args` key mixes positional and named arguments. Unknown or duplicate names give -8.
  - Exceptions map to -1 (`RPC_MISC_ERROR`) or -3 (type error).
  - `help [command] [subcommand]`.
  - `RPCHelpMan` validates arguments; `-rpcdoccheck` checks the documentation.
- **Error codes** (`rpc/protocol.h`). There are no Dash-specific codes.

  | Group | Codes |
  |---|---|
  | JSON-RPC standard | -32600 INVALID_REQUEST, -32601 METHOD_NOT_FOUND, -32602 INVALID_PARAMS, -32603 INTERNAL_ERROR, -32700 PARSE_ERROR |
  | General | -1 MISC, -2 FORBIDDEN_BY_SAFE_MODE (reserved), -3 TYPE, -5 INVALID_ADDRESS_OR_KEY, -7 OUT_OF_MEMORY, -8 INVALID_PARAMETER, -20 DATABASE, -22 DESERIALIZATION, -25 VERIFY_ERROR, -26 VERIFY_REJECTED, -27 ALREADY_IN_CHAIN, -28 IN_WARMUP, -32 METHOD_DEPRECATED |
  | P2P / chain | -9 CLIENT_NOT_CONNECTED, -10 IN_INITIAL_DOWNLOAD, -23 NODE_ALREADY_ADDED, -24 NODE_NOT_ADDED, -29 NODE_NOT_CONNECTED, -30 INVALID_IP_OR_SUBNET, -31 P2P_DISABLED, -33 MEMPOOL_DISABLED, -34 NODE_CAPACITY_REACHED |
  | Wallet | -4 WALLET_ERROR, -6 INSUFFICIENT_FUNDS, -11 INVALID_LABEL_NAME, -12 KEYPOOL_RAN_OUT, -13 UNLOCK_NEEDED, -14 PASSPHRASE_INCORRECT, -15 WRONG_ENC_STATE, -16 ENCRYPTION_FAILED, -17 ALREADY_UNLOCKED, -18 NOT_FOUND, -19 NOT_SPECIFIED, -35 ALREADY_LOADED, -36 ALREADY_EXISTS |

  HTTP status codes used: 200, 400, 401, 403, 404, 405, 500, 503.
- **`-deprecatedrpc` values** in use:

  | Value | Effect | Where |
  |---|---|---|
  | `service` | Masternode `service` / platform port fields | masternode.cpp:743, quorums.cpp:220, coinjoin client, evo/netinfo.cpp:73 |
  | `legacy_mn` | Legacy BLS `protx` variants | evo.cpp:620-2363 |
  | `softforks` | `getblockchaininfo.softforks` | blockchain.cpp:1626 |
  | `banscore` | `getpeerinfo` banscore | net.cpp:271 |
  | `walletwarningfield` | `warning` string in create/load/unload wallet | — |
  | `permissive_bool` | Accepts yes/no/1/0 strings as booleans | rpc/util.cpp:116 |

#### 3. REST (`src/rest.cpp`)

Enabled with `-rest`. Format suffixes are `.bin`, `.hex` and `.json`. Every endpoint returns 503 during warmup.

| Endpoint | Formats | Notes |
|---|---|---|
| `/rest/tx/<txid>.<ext>` | bin, hex, json | |
| `/rest/block/<hash>.<ext>` | bin, hex, json | |
| `/rest/block/notxdetails/<hash>.<ext>` | bin, hex, json | |
| `/rest/blockfilter/<type>/<hash>.<ext>` | bin, hex, json | |
| `/rest/blockfilterheaders/<type>/<hash>.<ext>?count=N` | bin, hex, json | Deprecated form `/<type>/<count>/<hash>`; default count 5, max 2000 |
| `/rest/headers/<hash>.<ext>?count=N` | bin, hex, json | Deprecated form `/<count>/<hash>`; default 5, 1-2000 |
| `/rest/chaininfo.json` | json | Same as `getblockchaininfo` |
| `/rest/deploymentinfo[/<hash>].json` | json | |
| `/rest/mempool/info.json` | json | |
| `/rest/mempool/contents.json?verbose=true\|false&mempool_sequence=` | json | `verbose` and `mempool_sequence` cannot both be true |
| `/rest/getutxos[/checkmempool]/<txid>-<n>/….<ext>` | bin, hex, json | Max 15 outpoints. bin/hex input may be in the POST body. Binary response = `height, hash, bitmap, outs` (BIP64) |
| `/rest/blockhashbyheight/<h>.<ext>` | bin, hex, json | |

#### 4. ZMQ (`src/zmq/`)

- Each message is three parts: `[topic string][body][uint32 LE sequence]`. The sequence counter is per notifier and kept only in memory (`zmqpublishnotifier.cpp:224-236`).
- Hashes are sent byte-reversed (display order). Raw bodies use network serialisation.
- Each topic has a matching `-zmqpub<topic>hwm` option, default 1000.

| Option topic | Wire topic | Body |
|---|---|---|
| pubhashblock | `hashblock` | 32-byte block hash |
| pubhashchainlock | `hashchainlock` | 32-byte chainlocked block hash |
| pubhashtx | `hashtx` | 32-byte txid |
| pubhashtxlock | `hashtxlock` | 32-byte txid (IS-locked) |
| pubhashgovernancevote | `hashgovernancevote` | 32-byte vote hash |
| pubhashgovernanceobject | `hashgovernanceobject` | 32-byte object hash |
| pubhashinstantsenddoublespend | `hashinstantsenddoublespend` | Two messages: current txid, then previous txid |
| pubhashrecoveredsig | `hashrecoveredsig` | 32-byte msgHash |
| pubrawblock | `rawblock` | Block |
| pubrawchainlock | `rawchainlock` | Block |
| pubrawchainlocksig | `rawchainlocksig` | Block followed by CLSIG |
| pubrawtx | `rawtx` | Transaction |
| pubrawtxlock | `rawtxlock` | Transaction |
| pubrawtxlocksig | `rawtxlocksig` | Transaction followed by ISLOCK |
| pubrawgovernancevote | `rawgovernancevote` | CGovernanceVote |
| pubrawgovernanceobject | `rawgovernanceobject` | Governance object |
| pubrawinstantsenddoublespend | `rawinstantsenddoublespend` | Two messages: current tx, then previous tx |
| pubrawrecoveredsig | `rawrecoveredsig` | CRecoveredSig |
| pubsequence | `sequence` | 32-byte hash, 1-byte label (`C` connect, `D` disconnect, `A` mempool add, `R` mempool remove), plus uint64 LE mempool sequence for A/R |

The `-help` text for `-zmqpubrawgovernancevote` and `-zmqpubrawgovernanceobject` is swapped (`init.cpp:733-734`). The RPC is `getzmqnotifications`.

#### 5. CLI tools

- **`dash-cli`**
  - `-getinfo` batches `getnetworkinfo`, `getblockchaininfo`, `getwalletinfo` and `getbalances`. It adds `mn_in`, `mn_out`, `mn_total` connections and `coinjoin_balance`. Without `-rpcwallet` it collects balances from every loaded wallet.
  - `-netinfo [0-4|help]` (max detail level 4).
  - `-addrinfo`.
  - `-generate [nblocks=1] [maxtries=1000000]` calls `getnewaddress` then `generatetoaddress`.
  - `-rpcwait` with `-rpcwaittimeout` (0 = wait forever).
  - `-stdin`, `-stdinrpcpass`, `-stdinwalletpassphrase`, `-named`, `-rpcwallet` (endpoint `/wallet/<name>`), `-color`.
  - Client HTTP timeout 900 s.
- **`dash-tx`** (`bitcoin-tx.cpp:46-72`)
  - Options: `-create`, `-json`, `-txid`.
  - Commands: `delin=N`, `delout=N`, `in=TXID:VOUT(:SEQ)`, `locktime=N`, `nversion=N` (1..3, `TX_MAX_STANDARD_VERSION`), `outaddr=`, `outdata=`, `outmultisig=VALUE:REQ:N:PK…[:S]`, `outpubkey=`, `outscript=`, `sign=SIGHASH` (needs registers `prevtxs` and `privatekeys`).
  - Register commands: `load=NAME:FILE`, `set=NAME:JSON`.
  - Also accepts the chain selection options.
- **`dash-util`**: command `grind <hex header>`.
- **`dash-wallet`**: see 1.13.
- **`dash-chainstate`**: `dash-chainstate DATADIR`, an experimental kernel driver (`bitcoin-chainstate.cpp`).

#### 6. Init and configuration

##### 6.1 Config semantics
- **Config file**: `dash.conf` in the base datadir. `-conf` is only valid on the command line. Writing `conf=` inside the file is an error.
- **Sections**: `[main]`, `[test]`, `[devnet]`, `[regtest]` (`util/system.cpp:209-214`). The devnet section is always named `devnet`, whatever the devnet name.
- **NETWORK_ONLY options** (`-addnode`, `-bind`, `-connect`, `-port`, `-rpcbind`, `-rpcport`, `-wallet`, `-walletdir`) set in the default section are ignored, with a warning, on any network other than main.
- **`includeconf`**:
  - Allowed only in the config file, including per-network sections; on the command line only `-noincludeconf` is accepted.
  - Relative to datadir.
  - Not recursive: an included file's `includeconf` is ignored with a warning.
- **Chain selection**: only one of `-regtest`, `-testnet`, `-devnet` or `-chain` may be given (`util/system.cpp:1020-1048`).
- **`settings.json`**: in the network datadir; disabled with `-nosettings`; read/write. Holds the `"wallet"` load-on-startup list and GUI-written settings.
- **Default datadir**:

  | Platform | Path |
  |---|---|
  | Unix | `~/.dashcore` |
  | macOS | `~/Library/Application Support/DashCore` |
  | Windows | `%APPDATA%\DashCore` |

- **Network subdirectory** (`chainparamsbase.cpp:39-51`): main `""`, test `testnet3`, regtest `regtest`, devnet `devnet-<name>` (or `devnet` when no name is given).
- **Datadir contents**: `blocks/`, `chainstate/`, `evodb/`, `llmq/` (with `dkgdb`, `isdb`, `quorumdb`, `recsigdb`), `indexes/` (`txindex`, `blockfilter`, `coinstats`, `addressindex`, `spentindex`, `timestampindex`), `wallets/`, `backups/`, `debug.log`, `dashd.pid`, `.cookie`, `settings.json`, `peers.dat`, `anchors.dat`, `banlist.json`, `mempool.dat`, `fee_estimates.dat`, `mncache.dat`, `governance.dat`, `netfulfilled.dat`, `sporks.dat`.
- **Ports**:

  | Network | P2P | RPC | Tor onion target |
  |---|---|---|---|
  | main | 9999 | 9998 | 9996 |
  | test | 19999 | 19998 | 19996 |
  | devnet | 19799 | 19798 | 19796 |
  | regtest | 19899 | 19898 | 19896 |

- **Address prefixes**: main P2PKH 76, P2SH 16, WIF 204, xpub `0488B21E`, xprv `0488ADE4`. All test networks: 140, 19, 239, `043587CF` / `04358394`.

##### 6.2 Option table

**Notes for the table:**
- Flags: D = `DEBUG_ONLY` (shown only with `-help-debug`), N = `NETWORK_ONLY`, S = `SENSITIVE`, c = compiled conditionally.
- Dash? Y = not in Bitcoin Core, or Dash-specific semantics.
- Fee amounts are in DASH/kB.
- Category OPT = OPTIONS. Unless another file is named, lines are in `init.cpp` (`SetupServerArgs`, :571-893).

| Option | Category | Default | Meaning | Dash? | Flags |
|---|---|---|---|---|---|
| -? / -h / -help | OPT | — | help (`-h`/`-help` are hidden aliases) | N | |
| -help-debug | DEBUG_TEST | — | show debug options | N | |
| -alertnotify=<cmd> | OPT | — | run cmd on alert (%s = message, single-quoted) | N | c |
| -assumevalid=<hex> | OPT | main 0x…19fe42827219e0686d3f7b494ae65f823194c740c5dbab492 (h2487500); test 0x…541a23f9db74…0239 (h1400000); devnet 0 | skip script checks below this block | N | |
| -blocksdir=<dir> | OPT | <datadir> | blocks directory | N | |
| -fastprune | DEBUG_TEST | 0 | small block files | N | D |
| -tinyblk | DEBUG_TEST | 0 | small block files | Y | D |
| -blocknotify=<cmd> | OPT | — | on tip change (%s = hash) | N | c |
| -blockreconstructionextratxn=<n> | OPT | 100 | compact block extra txs | N | |
| -blocksonly | OPT | 0 | reject p2p txs | N | |
| -chainlocknotify=<cmd> | OPT | — | on best chainlock (%s = hash) | Y | c |
| -coinstatsindex | OPT | 0 | coinstats index | N | |
| -conf=<file> | OPT | dash.conf | config file | N | |
| -datadir=<dir> | OPT | see 6.1 | data dir | N | |
| -dbbatchsize | OPT | 16777216 | DB write batch bytes | N | D |
| -dbcache=<n> | OPT | 300 (4..16384) | MiB | N | |
| -includeconf=<file> | OPT | — | extra config (file only) | N | |
| -allowignoredconf | OPT | 0 | warn instead of error on unused dash.conf | N | |
| -loadblock=<file> | OPT | — | import blocks | N | |
| -maxmempool=<n> | OPT | 300 | MB | N | |
| -maxorphantxsize=<n> | OPT | 10 | total orphan MB | Y | |
| -maxrecsigsage=<n> | OPT | 604800 | seconds to keep LLMQ recovered sigs | Y | |
| -mempoolexpiry=<n> | OPT | 336 | hours | N | |
| -minimumchainwork=<hex> | OPT | main 0x…ba7225adc3153945d14d; test 0x…036c8f738da818d2; devnet 0 | min chain work | N | D |
| -par=<n> | OPT | 0 (max 15) | script verification threads | N | |
| -parbls=<n> | OPT | 0 (max 33) | BLS verification threads | Y | |
| -persistmempool | OPT | 1 | save/load mempool | N | |
| -pid=<file> | OPT | dashd.pid | pid file | N | |
| -prune=<n> | OPT | 0 (1 = manual; >945 = MiB target) | incompatible with txindex/addressindex/spentindex/governance | N (incompat. Y) | |
| -settings=<file> | OPT | settings.json | dynamic settings | N | |
| -syncmempool | OPT | 1 | sync mempool from peers at start | Y | |
| -startupnotify=<cmd> | OPT | — | run on startup | N | c |
| -shutdownnotify=<cmd> | OPT | — | run before shutdown (joined) | N | c |
| -version | OPT | — | print version | N | |
| -addressindex | INDEXING | 0 | address index | Y | |
| -reindex | INDEXING | — | rebuild block index and indexes | N | |
| -reindex-chainstate | INDEXING | — | rebuild chainstate | N | |
| -spentindex | INDEXING | 0 | spent index | Y | |
| -timestampindex | INDEXING | 0 | timestamp index | Y | |
| -txindex | INDEXING | **1** | tx index | N (default differs) | |
| -blockfilterindex=<type> | OPT | 0 (values: basic) | auto `basic` for masternodes | N | |
| -asmap=<file> | CONNECTION | ip_asn.map | ASN map | N | |
| -addnode=<ip> | CONNECTION | — | max 8 | N | N |
| -allowprivatenet | CONNECTION | 0 | allow RFC1918 addresses | Y | |
| -bantime=<n> | CONNECTION | 86400 | manual ban seconds | N | |
| -bind=<addr>[:port][=onion] | CONNECTION | 0.0.0.0; onion 127.0.0.1:9996/19996/19796/19896 | listen | N | N |
| -cjdnsreachable | CONNECTION | 0 | CJDNS | N | |
| -connect=<ip> | CONNECTION | — | connect only to | N | N |
| -discover | CONNECTION | 1 (listening, no externalip/proxy) | discover own IP | N | |
| -dns | CONNECTION | 1 | DNS lookups | N | |
| -dnsseed | CONNECTION | 1 | DNS seeding | N | |
| -externalip=<ip> | CONNECTION | — | own address | N | |
| -fixedseeds | CONNECTION | 1 | fixed seeds | N | |
| -forcednsseed | CONNECTION | 0 | always DNS-seed | N | |
| -listen | CONNECTION | 1 | accept inbound | N | |
| -listenonion | CONNECTION | 1 | Tor hidden service | N | |
| -maxconnections=<n> | CONNECTION | 125 | peer limit | N | |
| -maxreceivebuffer=<n> | CONNECTION | 5000 (×1000 B) | | N | |
| -maxsendbuffer=<n> | CONNECTION | 1000 (×1000 B) | | N | |
| -maxtimeadjustment | CONNECTION | 4200 s | | N | |
| -maxuploadtarget=<n> | CONNECTION | 0M | per 24h | N | |
| -onion=<ip:port\|path> | CONNECTION | -proxy | Tor proxy | N | |
| -i2psam=<ip:port> | CONNECTION | none | I2P SAM | N | |
| -i2pacceptincoming | CONNECTION | 1 | | N | |
| -onlynet=<net> | CONNECTION | — | ipv4, ipv6, onion, i2p, cjdns | N | |
| -v2transport | CONNECTION | 1 | BIP324 | N | |
| -peerblockfilters | CONNECTION | 0 (auto for MN) | BIP157 | N | |
| -peerbloomfilters | CONNECTION | 1 | BIP37 | N | |
| -txreconciliation | CONNECTION | 0 | BIP330 | N | D |
| -peertimeout=<n> | CONNECTION | 60 | | N | |
| -port=<port> | CONNECTION | 9999/19999/19799/19899 | | N | N |
| -proxy=<ip:port\|path> | CONNECTION | disabled | SOCKS5 | N | DISALLOW_ELISION |
| -proxyrandomize | CONNECTION | 1 | | N | |
| -seednode=<ip> | CONNECTION | — | | N | |
| -socketevents=<mode> | CONNECTION | epoll (Linux), kqueue (BSD/mac), select (Windows) | select/poll/epoll/kqueue | Y | |
| -networkactive | CONNECTION | 1 | | N | |
| -timeout=<n> | CONNECTION | 5000 ms | connect timeout | N | |
| -torcontrol=<ip>:<port> | CONNECTION | 127.0.0.1:9051 | | N | |
| -torpassword=<pass> | CONNECTION | "" | | N | S |
| -upnp | CONNECTION | 0 | | N | c |
| -natpmp | CONNECTION | 0 | | N | c |
| -whitebind=<[perm@]addr> | CONNECTION | perms download,noban,mempool,relay | permissions: bloomfilter, noban, forcerelay, relay, mempool, download, addr | N | |
| -whitelist=<[perm@]net> | CONNECTION | — | | N | |
| -zmqpub{hashblock,hashchainlock,hashgovernanceobject,hashgovernancevote,hashinstantsenddoublespend,hashrecoveredsig,hashtx,hashtxlock,rawblock,rawchainlock,rawchainlocksig,rawgovernancevote,rawgovernanceobject,rawinstantsenddoublespend,rawrecoveredsig,rawtx,rawtxlock,rawtxlocksig,sequence}=<addr> | ZMQ | — | see section 4 (19 options) | Y except hashblock, hashtx, rawblock, rawtx, sequence | c |
| -zmqpub<same 19>hwm=<n> | ZMQ | 1000 | high water mark | same | c |
| -checkblockindex | DEBUG_TEST | 0 (regtest 1) | | N | D |
| -checkblocks=<n> | DEBUG_TEST | 6 | | N | D |
| -checklevel=<n> | DEBUG_TEST | 3 (0-4) | | N | D |
| -checkaddrman=<n> | DEBUG_TEST | 0 | | N | D |
| -checkmempool=<n> | DEBUG_TEST | 0 (regtest 1) | | N | D |
| -checkpoints | DEBUG_TEST | 1 (main up to 2487500) | | Y (removed upstream) | D |
| -deprecatedrpc=<method> | DEBUG_TEST | — | values in section 2 | N | D |
| -forceevodbrepair | DEBUG_TEST | 0 | evodb diff repair | Y | D |
| -limitancestorcount=<n> | DEBUG_TEST | 25 | | N | D |
| -limitancestorsize=<n> | DEBUG_TEST | 101 kB | | N | D |
| -limitdescendantcount=<n> | DEBUG_TEST | 25 | | N | D |
| -limitdescendantsize=<n> | DEBUG_TEST | 101 kB | | N | D |
| -stopafterblockimport | DEBUG_TEST | 0 | | N | D |
| -stopatheight | DEBUG_TEST | 0 | | N | D |
| -watchquorums=<n> | DEBUG_TEST | 0 | watch quorum communication | Y | D |
| -capturemessages | DEBUG_TEST | — | dump p2p messages | N | D |
| -disablegovernance | DEBUG_TEST | 0 | | Y | |
| -maxsigcachesize=<n> | DEBUG_TEST | 32 MiB | | N | D |
| -maxtipage=<n> | DEBUG_TEST | 21600 s | | N (value differs) | D |
| -mocktime=<n> | DEBUG_TEST | 0 | | N | D |
| -printpriority | DEBUG_TEST | 0 | | N | D |
| -pushversion | DEBUG_TEST | — | override protocol version | Y | |
| -sporkaddr=<addr> | DEBUG_TEST | — | regtest/devnet spork address | Y | |
| -sporkkey=<privkey> | DEBUG_TEST | — | spork signing key | Y | S |
| -uacomment=<cmt> | DEBUG_TEST | — | | N | |
| -acceptstalefeeestimates | DEBUG_TEST | 0 (regtest only) | stale = 60h | N | D |
| -debuglogfile=<file> (init/common.cpp:29) | OPT | debug.log | | N | |
| -debug=<cat> | DEBUG_TEST | off | categories: net tor mempool http bench zmq walletdb rpc estimatefee addrman selectcoins reindex cmpctblock rand prune proxy mempoolrej libevent coindb qt leveldb validation ipc lock blockstorage txreconciliation scan all, plus Dash: chainlocks gobject instantsend llmq llmq-dkg llmq-sigs mnpayments mnsync coinjoin spork netconn creditpool ehf dash | N/Y | |
| -debugexclude=<cat> | DEBUG_TEST | — | | N | |
| -logips | DEBUG_TEST | 0 | | N | |
| -loglevel=<lvl>\|<cat>:<lvl> | DEBUG_TEST | debug (info, debug, trace) | | N | D, DISALLOW_NEGATION, DISALLOW_ELISION |
| -logtimestamps | DEBUG_TEST | 1 | | N | |
| -logthreadnames | DEBUG_TEST | 0 | | N | D |
| -logsourcelocations | DEBUG_TEST | 0 | | N | |
| -logtimemicros | DEBUG_TEST | 0 | | N | D |
| -loglevelalways | DEBUG_TEST | 0 | | N | |
| -printtoconsole | DEBUG_TEST | 1 when not -daemon | | N | |
| -shrinkdebugfile | DEBUG_TEST | 1 when no -debug | | N | |
| -chain=<chain> (chainparamsbase.cpp:20) | CHAINPARAMS | main | main/test/devnet/regtest | N (devnet Y) | |
| -devnet=<name> | CHAINPARAMS | — | devnet | Y | |
| -regtest | CHAINPARAMS | — | | N | D |
| -testnet | CHAINPARAMS | — | | N | |
| -budgetparams=<mn>:<budget>:<sb> (chainparams.cpp:1390) | CHAINPARAMS | — | regtest | Y | D |
| -dip3params=<act>:<enf> | CHAINPARAMS | — | regtest | Y | D |
| -highsubsidyblocks=<n> | CHAINPARAMS | 0 | devnet | Y | |
| -highsubsidyfactor=<n> | CHAINPARAMS | 1 | devnet | Y | |
| -llmqchainlocks=<name> | CHAINPARAMS | llmq_devnet | devnet | Y | |
| -llmqdevnetparams=<size>:<thr> | CHAINPARAMS | — | devnet | Y | |
| -llmqinstantsenddip0024=<name> | CHAINPARAMS | llmq_devnet_dip0024 | devnet | Y | |
| -llmqplatform=<name> | CHAINPARAMS | llmq_devnet_platform | devnet | Y | |
| -llmqmnhf=<name> | CHAINPARAMS | llmq_devnet | devnet | Y | |
| -llmqtestinstantsenddip0024=<name> | CHAINPARAMS | llmq_test_dip0024 | regtest | Y | D |
| -llmqtestinstantsendparams=<s>:<t> | CHAINPARAMS | 3:2 | regtest | Y | D |
| -llmqtestparams=<s>:<t> | CHAINPARAMS | 3:2 | regtest | Y | D |
| -llmqtestplatformparams=<s>:<t> | CHAINPARAMS | 3:2 | regtest | Y | D |
| -minimumdifficultyblocks=<n> | CHAINPARAMS | 0 | devnet | Y | |
| -powtargetspacing=<n> | CHAINPARAMS | 150 s | devnet | Y | DISALLOW_NEGATION |
| -testactivationheight=name@h | CHAINPARAMS | — | bip147, bip34, dersig, cltv, csv, brr, dip0001, dip0008, dip0024, v19, v20, mn_rr (regtest) | Y (names) | D |
| -vbparams=dep:start:end(:minact(:win:thr(:thrmin:falloff:mnact))) | CHAINPARAMS | — | regtest | Y (extended) | D |
| -llmq-data-recovery=<n> | MASTERNODE | 1 | quorum data recovery | Y | |
| -llmq-qvvec-sync=<name>:<mode> | MASTERNODE | — | mode 0 = always, 1 = only if member | Y | |
| -masternodeblsprivkey=<hex> | MASTERNODE | — | enables masternode mode | Y | S |
| -deprecated-platform-user=<user> | MASTERNODE | — | no-op (unused) | Y | |
| -acceptnonstdtxn | NODE_RELAY | 0 | test networks only | N | D |
| -dustrelayfee=<amt> | NODE_RELAY | 0.00003 | | N | D |
| -incrementalrelayfee=<amt> | NODE_RELAY | 0.00001 | | N | D |
| -bytespersigop | NODE_RELAY | 20 | | N | |
| -datacarrier | NODE_RELAY | 1 | | N | |
| -datacarriersize | NODE_RELAY | 83 | | N | |
| -permitbaremultisig | NODE_RELAY | 1 | | N | |
| -minrelaytxfee=<amt> | NODE_RELAY | 0.00001 | | N | |
| -whitelistforcerelay | NODE_RELAY | 0 | | N | |
| -whitelistrelay | NODE_RELAY | 1 | | N | |
| -blockmaxsize=<n> | BLOCK_CREATION | 2000000 | | Y | |
| -blockmintxfee=<amt> | BLOCK_CREATION | 0.00001 | | N | |
| -blockversion=<n> | BLOCK_CREATION | — | | N | D |
| -rest | RPC | 0 | | N | |
| -rpcallowip=<ip> | RPC | — | | N | |
| -rpcauth=<user:salt$hash> | RPC | — | | N | S |
| -rpcbind=<addr>[:port] | RPC | 127.0.0.1 and ::1 | | N | N |
| -rpcdoccheck | RPC | 0 (1 if built with RPC_DOC_CHECK) | | N | D |
| -rpccookiefile=<loc> | RPC | <netdatadir>/.cookie | | N | |
| -rpcexternaluser=<users> | RPC | — | comma list, separate queue | Y | S |
| -rpcexternalworkqueue=<n> | RPC | 16 | | Y | D |
| -rpcpassword=<pw> | RPC | — | | N | S |
| -rpcport=<port> | RPC | 9998/19998/19798/19898 | | N | N |
| -rpcservertimeout=<n> | RPC | 30 | | N | D |
| -rpcthreads=<n> | RPC | 4 | | N | |
| -rpcuser=<user> | RPC | — | | N | S |
| -rpcwhitelist=<user:rpcs> | RPC | — | | N (composite Y) | |
| -rpcwhitelistdefault | RPC | = IsArgSet(-rpcwhitelist) | | N | |
| -rpcworkqueue=<n> | RPC | 16 | | N | D |
| -server | RPC | 1 for dashd (0 for GUI) | | N | |
| -statsbatchsize=<bytes> | STATSD | 1024 | | Y | |
| -statsduration=<ms> | STATSD | 1000 | flush interval | Y | |
| -statshost=<ip\|udp://host:port> | STATSD | "" (disabled) | | Y | |
| -statsport | hidden | 8125 | | Y | |
| -statsperiod=<s> | STATSD | 60 (clamped 5..3600) | | Y | |
| -statsprefix=<s> | STATSD | "" | `prefix.` | Y | |
| -statssuffix=<s> | STATSD | "" | `.suffix` | Y | |
| -daemon | OPT | 0 | | N | c |
| -daemonwait | OPT | 0 | | N | c |
| -dbcrashratio, -forcecompactdb, -printcrashinfo | hidden | — | test hooks | N | |
| **Wallet (wallet/init.cpp:61-135)** | | | | | |
| -avoidpartialspends | WALLET | 0 | | N | |
| -consolidatefeerate=<amt> | WALLET | 0.00001 | | N | |
| -createwalletbackups=<n> | WALLET | 10 | auto-backup count | Y | |
| -disablewallet | WALLET | 0 | | N | |
| -instantsendnotify=<cmd> | WALLET | — | %s = txid, %w = wallet | Y | c |
| -keypool=<n> | WALLET | 1000 | | N | |
| -signer=<cmd> | WALLET | — | | N | c |
| -spendzeroconfchange | WALLET | 1 | | N | |
| -dustprotectionthreshold=<n> | WALLET | 0 (max 1000000 duffs) | | Y | |
| -wallet=<path> | WALLET | — | | N | N |
| -walletbackupsdir=<dir> | WALLET | <netdatadir>/backups | | Y | |
| -walletbroadcast | WALLET | 1 | | N | |
| -walletdir=<dir> | WALLET | <datadir>/wallets if it exists, else <datadir> | | N | N |
| -walletnotify=<cmd> | WALLET | — | %s txid, %w wallet, %b block hash or "unconfirmed", %h height or -1 | N | c |
| -discardfee=<amt> | WALLET_FEE | 0.0001 | | N | |
| -fallbackfee=<amt> | WALLET_FEE | 0.00001 | | N (value differs) | |
| -maxapsfee=<n> | WALLET | 0.00 | | N | |
| -maxtxfee=<amt> | DEBUG_TEST | 0.10 | | N | |
| -mintxfee=<amt> | WALLET_FEE | 0.00001 | | N | |
| -paytxfee=<amt> | WALLET_FEE | 0.00 | | N | |
| -txconfirmtarget=<n> | WALLET_FEE | 6 | | N | |
| -hdseed=<hex> | WALLET_HD | random | legacy only; overrides mnemonic | Y | S |
| -mnemonic=<text> | WALLET_HD | random | | Y | S |
| -mnemonicbits=<n> | WALLET_HD | 128 | | Y | |
| -mnemonicpassphrase=<text> | WALLET_HD | "" | | Y | S |
| -usehd | WALLET_HD | 1 | | Y | |
| -enablecoinjoin | WALLET_COINJOIN | 0 | | Y | |
| -coinjoinamount=<n> | WALLET_COINJOIN | 1000 (2..21000000) | | Y | |
| -coinjoinautostart | WALLET_COINJOIN | 0 | | Y | |
| -coinjoindenomsgoal=<n> | WALLET_COINJOIN | 50 (10..100000) | | Y | |
| -coinjoindenomshardcap=<n> | WALLET_COINJOIN | 300 (10..100000) | | Y | |
| -coinjoinmultisession | WALLET_COINJOIN | 0 | | Y | |
| -coinjoinrounds=<n> | WALLET_COINJOIN | 4 (2..16) | | Y | |
| -coinjoinsessions=<n> | WALLET_COINJOIN | 4 (1..10) | | Y | |
| -dblogsize=<n> | WALLET_DEBUG_TEST | 100 | BDB builds only | N | D, c |
| -flushwallet | WALLET_DEBUG_TEST | 1 | BDB builds only | N | D, c |
| -privdb | WALLET_DEBUG_TEST | 1 | BDB builds only | N | D, c |
| -unsafesqlitesync | WALLET_DEBUG_TEST | false | SQLite builds only | N | D, c |
| -walletrejectlongchains | WALLET_DEBUG_TEST | 1 | | N | D |
| -walletcrosschain | WALLET_DEBUG_TEST | 0 | | N | D |
| **GUI (qt/bitcoin.cpp:489-505)** | | | | | |
| -choosedatadir | GUI | 0 | | N | |
| -custom-css-dir | GUI | — | | Y | |
| -font-family | GUI | OS font | | Y | |
| -font-scale | GUI | 0 (-100..100) | | Y | |
| -font-weight-bold / -font-weight-normal | GUI | runtime (0..8) | | Y | |
| -lang=<lang> | GUI | system locale | | N | |
| -min | GUI | — | | N | |
| -resetguisettings | GUI | — | | N | |
| -splash | GUI | 1 | | N | |
| -uiplatform | GUI | platform | windows/macosx/other | Y | D |
| -debug-ui | GUI | 0 | | Y | D |
| -windowtitle=<name> | GUI | — | | Y | |
| **dash-cli (bitcoin-cli.cpp:78-104)** | | | | | |
| -version, -conf, -datadir, chain options | OPT | dash.conf | | N | |
| -generate [n] [maxtries] | OPT | 1 / 1000000 | | N | |
| -addrinfo, -getinfo, -netinfo [0-4\|help] | OPT | 0 | | N (getinfo has MN/CJ fields) | |
| -color=<when> | OPT | auto | | N | DISALLOW_NEGATION |
| -named | OPT | 0 | | N | |
| -rpcclienttimeout=<n> | OPT | 900 | | N | |
| -rpcconnect=<ip> | OPT | 127.0.0.1 | | N | |
| -rpccookiefile, -rpcpassword, -rpcuser | OPT | — | | N | |
| -rpcport | OPT | 9998/19998/19798/19898 | | N | N |
| -rpcwait | OPT | — | | N | |
| -rpcwaittimeout=<n> | OPT | 0 | | N | DISALLOW_NEGATION |
| -rpcwallet=<name> | OPT | — | | N | |
| -stdin, -stdinrpcpass, -stdinwalletpassphrase | OPT | — | | N | |
| **dash-wallet (bitcoin-wallet.cpp:33-42)** | | | | | |
| -version, -datadir, chain options | OPT | — | | N | |
| -usehd | OPT | 1 | | Y | |
| -wallet=<name> | OPT | — | | N | |
| -dumpfile=<file> | OPT | — | | N | DISALLOW_NEGATION |
| -debug, -printtoconsole | DEBUG_TEST | — | | N | |
| -descriptors / -legacy | OPT | descriptors when neither given | | N | |
| -format=bdb\|sqlite | OPT | from dump file | | N | |
| **dash-tx / dash-util** | | | | | |
| dash-tx: -version, -create, -json, -txid, plus commands in section 5 | | | | | |
| dash-util: -version, `grind` | | | | | |

#### 7. Stats client (`src/stats/`)
- **Transport**: UDP, batched. `-statshost` may be a bare host or `udp://host:port`; a port in the URL overrides `-statsport` (default 8125).
- **Message format**: `<prefix.>key<.suffix>:%f|<c|g|ms>[|@rate]`, newline-separated, batches of `-statsbatchsize` bytes flushed every `-statsduration` ms (`client.cpp:25-40, 200-260`). Periodic stats run every `-statsperiod` seconds (`init.cpp:2384`).
- **Metrics emitted:**
  - **Timings**: `AcceptBlock_us`, `AcceptToMemoryPool_ms`, `ActivateBestChain_ms`, `CheckBlock_us`, `CheckInputScripts_ms`, `ConnectBlock_ms`, `ConnectTip_ms`, `DisconnectBlock_ms`, `DisconnectTip_ms`, `islock_ms`, `peers.ping_us`.
  - **Counters (count)**: `bandwidth.bytesReceived` / `bytesSent`, `bandwidth.message.<type>.bytesReceived` / `.bytesSent`, `misbehavior.amount`, `transactions.{fees,inputValue,inputs,outputValue,outputs,sigOps,sizeBytes}`.
  - **Counters (inc)**: `message.received.<type>`, `message.received.inv_<type>`, `message.sent.<type>`, `misbehavior.banned`, `peers.connect` / `disconnect`, `transactions.{accepted,duplicate,orphans.add,orphans.remove}`, `warnings.{ConflictingChainFound,InvalidBlockFound,InvalidChainFound}`.
  - **Gauges**:
    - `bandwidth.message.<msg>.totalBytesReceived` / `Sent`, `bandwidth.totalBytesReceived` / `Sent`
    - `blocks.tip.{Height,NumTransactions,SigOps,SizeBytes,Version}`, `chainlocks.blockHeight`
    - `masternodes.{count,enabled,evo.count,evo.enabled,mn.count,mn.enabled,weighted_count,weighted_enabled}`
    - `peers.{fullNodeConnections,inboundConnections,ipv4Connections,ipv6Connections,outboundConnections,spvNodeConnections,torConnections,totalConnections}`
    - `transactions.mempool.{lockedTransactions,memoryUsageBytes,minFeePerKb,totalTransactions,totalTxBytes}`, `transactions.{orphans,totalTransactions,txCacheSize}`
    - `utxoset.{blockHeight,dbSizeBytes,totalAmount,tx,txOutputs}`
    - `network.{difficulty,hashesPerSecond,terahashesPerSecond,petahashesPerSecond,exahashesPerSecond}` (double)

#### 8. Notification options
Each runs `runCommand` on a detached thread.

| Option | Trigger | Substitutions | Where |
|---|---|---|---|
| `-blocknotify` | tip change after init | %s = hash | init.cpp:2429 |
| `-chainlocknotify` | new best chainlock | %s = hash | init.cpp:2439 |
| `-alertnotify` | alert | %s = sanitised, single-quoted message | validation.cpp:2000 |
| `-walletnotify` | wallet tx change | %s, %w, %b, %h | wallet.cpp:1138 |
| `-instantsendnotify` | wallet tx IS-locked | %s, %w | wallet.cpp:3872 |
| `-startupnotify` | startup | — | init.cpp:914 |
| `-shutdownnotify` | before shutdown; repeatable; threads are joined | — | init.cpp:270 |

`%w` is not substituted on Windows. All of these require `HAVE_SYSTEM`.

#### 9. Qt GUI (`src/qt`), brief
- **Main tabs**: Overview (balances, CoinJoin/mixing panel), Send, CoinJoin send tab (fully mixed coins only), Receive, Transactions, Masternodes (list, registration wizard, shared masternode dialogs), Governance (proposal list, create, info, vote, resume).
- **Menu actions**: encrypt wallet, change passphrase, backup, Show Recovery Phrase (mnemonic) with mnemonic verification on create, unlock/lock, sign/verify message, load PSBT from file or clipboard, sending/receiving address books, Open URI, create/open/close/restore/migrate wallet, Show Automatic Backups, open wallet config file, open debug log, discreet mode.
- **Information/Debug window tabs**: General, Console, Network Traffic, Peers, Network, Repair (rescan from tip or genesis; restart with `-reindex`).
- **Options dialog tabs**: Main, Wallet (coin control, spend unconfirmed change, CoinJoin, dust protection, external signer path), Network, Display, Appearance (fonts and themes).
- **Other**: Network Monitor, coin control dialog, QR codes, payment server for `dash:` URIs, splash screen, tray icon.

---

## B. RPC inventory (summary)

**The full table** (category | command | purpose | user-story bucket | source | notes) is in [`R2b-rpc-inventory.md`](R2b-rpc-inventory.md). It was generated by parsing every `CRPCCommand` table under `src/`; the source files are listed at the end of this section.

**Counts:**

| Measure | Value |
|---|---|
| Table rows | 292 |
| Distinct command names | 289 (`protx list`, `protx info` and `getcoinjoininfo` are registered twice, once node-side and once wallet-aware) |
| Dash-specific commands | 115 |

| Category | Rows |
|---|---|
| wallet | 71 |
| evo (`protx`, `quorum`, `bls`, ChainLock/IS verify, quorum proofs) | 51 |
| dash (`masternode`, `gobject`, `coinjoin`, `spork`, `mnsync`, …) | 39 |
| blockchain | 36 |
| hidden / test-only | 32 |
| rawtransactions | 21 |
| network | 13 |
| util | 8 |
| control | 7 |
| mining | 7 |
| addressindex | 5 |
| zmq | 1 |
| signer | 1 |

**Commands that have no Bitcoin Core equivalent, by bucket** (each needs its own Rust implementation):

- **Masternode operator.** One `protx` command per ProTx operation:
  - registration: `protx register` / `register_fund` / `register_prepare` / `register_submit`, and their `*_evo` and `*_legacy` variants
  - updates: `protx update_service`, `protx update_service_evo`, `protx update_registrar`, `protx revoke`
  - shared masternodes: `protx shared_register_prepare` / `shared_sign` / `shared_combine` / `shared_dissolve` / `shared_dissolve_prepare` / `shared_update_share` / `shared_update_registrar_prepare`
  - queries: `protx list` / `info` / `diff` / `listdiff`
  - also: `masternode outputs` / `status` / `count` / `list` / `payments` / `winners` / `connect`, `masternodelist`, `bls generate` / `fromsecret`, `quorum memberof` / `dkgstatus` / `dkginfo`
- **Governance:** `gobject prepare` / `list-prepared` / `submit` / `check` / `deserialize` / `count` / `list` / `diff` / `get` / `getcurrentvotes` / `vote-many` / `vote-alias`, `voteraw`, `getgovernanceinfo`, `getsuperblockbudget`.
- **Mixing:** `coinjoin start` / `stop` / `reset` / `status`, `coinjoinsalt generate` / `get` / `set`, `getcoinjoininfo`, `setcoinjoinrounds`, `setcoinjoinamount`.
- **Platform:**
  - quorum signing and lookup: `quorum sign` / `platformsign` / `verify` / `hasrecsig` / `getrecsig` / `isconflicting` / `selectquorum` / `list` / `listextended` / `info` / `rotationinfo`
  - ChainLock and InstantSend: `submitchainlock`, `verifychainlock`, `verifyislock`, `getchainlockbyheight`, `getbestchainlock`
  - quorum proofs: `getquorumproofchain`, `verifyquorumproofchain` (DASHNC02 proofs)
  - credits: `getcreditpoolinfo`, `getassetunlockstatuses`
- **Indexes and integrators:** `getaddressbalance` / `utxos` / `deltas` / `txids` / `mempool`, `getspentinfo`, `getblockhashes`, `getrawtransactionmulti`, `getislocks`, `gettxchainlocks`, `getspecialtxes`, `getblockheaders`, `getmerkleblocks`.
- **Wallet extras:** `dumphdinfo`, `upgradetohd`, `importelectrumwallet`, `listaddressbalances`, `wipewallettxes`.
- **Node operations and debugging:** `mnsync`, `spork`, `sporkupdate`, `mnauth`, `setmnthreadactive`, `evodb verify` / `repair`, `quorum dkgsimerror`, `quorum getdata`.

**Compatibility rules to replicate:**

1. **Subcommand dispatch.** `CRPCTable::execute` (`src/rpc/server.cpp:496-526`) first looks up `method + " " + params[0]` and, if found, drops the first param.
2. **Whitelist matching.** `-rpcwhitelist` matches a subcommand as `method + params[0]` with no space, e.g. `quorumsign` (`httprpc.cpp:106-112`).
3. **Wallet-only registration.** Wallet-gated tables are registered only when the wallet is enabled. They are reachable via `/wallet/<name>`.
4. **Deprecated variants.** `-deprecatedrpc` values that change output: `service`, `legacy_mn`, `softforks`, `banscore`, `walletwarningfield`, `permissive_bool` (A.6 §2).
5. **Error codes.** These are Bitcoin's; Dash adds none (A.6 §2).
6. **Strings and numbers.** Some protx and gobject arguments accept either a string or a number (`skip_type_check`).
7. **Output shapes.** Dash-specific JSON shapes are produced in `src/evo/core_write.cpp`, `src/llmq/core_write.cpp`, `src/governance/common.cpp` (`ToJson`) and `src/core_write.cpp`. Examples: the `proRegTx` / `cbTx` / `qcTx` / `assetLockTx` objects in `decoderawtransaction`, and the `instantlock` / `chainlock` fields on `getrawtransaction` and `getblock`.

**Source files containing the parsed `CRPCCommand` tables:** `src/rpc/{server,blockchain,mempool,mining,net,node,rawtransaction,output_script,fees,signmessage,txoutproof,external_signer,masternode,coinjoin,governance,evo,quorums}.cpp`, `src/wallet/rpc/wallet.cpp`, `src/zmq/zmqrpc.cpp`.

---

## C. Consensus parameter & activation surface

Sources: `src/chainparams.cpp` (1409 lines), `src/chainparamsbase.cpp`, `src/consensus/params.h`, `src/consensus/consensus.h`, `src/llmq/params.h`, `src/deploymentinfo.cpp`, `src/versionbits.cpp`, `src/deploymentstatus.{h,cpp}`. Four chain classes: `CMainParams`, `CTestNetParams`, `CDevNetParams` (parameterised by `-devnet=<name>` and many devnet-only overrides), `CRegTestParams` (parameterised by test-only overrides). Client version on develop is **24.1.0 (pre-release)**, `PROTOCOL_VERSION = 70242`.

### C.1 Network identity

| | mainnet | testnet3 | devnet | regtest |
|---|---|---|---|---|
| `-chain` / datadir subdir | `main` / `` (root) | `test` / `testnet3` | `devnet` / `devnet-<name>` (from `GetDevNetName()`) | `regtest` / `regtest` |
| pchMessageStart | `bf 0c 6b bd` | `ce e2 ca ff` | `e2 ca ff ce` | `fc c1 b7 dc` |
| P2P port | 9999 | 19999 | 19799 | 19899 |
| RPC port | 9998 | 19998 | 19798 | 19898 |
| Tor onion target port | 9996 | 19996 | 19796 | 19896 |
| Platform P2P / HTTP default ports | 26656 / 443 | 22000 / 22001 | 22100 / 22101 | 22200 / 22201 |
| Genesis | `00000ffd590b1485…07ab6` (t=1390095618, nonce 28917698, bits 0x1e0ffff0, 50 DASH) | `00000bafbc94add7…bc2c` (t=1390666206, nonce 3861367235) | regtest genesis + **devnet genesis** at height 1 (`CreateDevNetGenesisBlock`: prev=genesis, coinbase scriptSig `1 <devnetName>`, OP_RETURN output, mined at bits of genesis; `hashDevnetGenesisBlock`) | t=1417713337, nonce 1096447, bits 0x207fffff |
| Genesis merkle root (all) | `e0028eb9648db56b1ac77cf090b99048a8007e2bb64b68f092c03c7f56a662c7` | same | same | same |
| Base58 P2PKH / P2SH / WIF | 76 ('X') / 16 ('7') / 204 | 140 ('y') / 19 ('8','9') / 239 | same as testnet | same as testnet |
| BIP32 xpub/xprv | `0488B21E` / `0488ADE4` | `043587CF` / `04358394` | testnet | testnet |
| BIP44 coin type | 5 | 1 | 1 | 1 |
| DIP-18 Platform bech32m HRP | `dash` | `tdash` | `tdash` | `tdash` |
| DNS seeds | `dnsseed.dash.org.` | `testnet-seed.dashdot.io.` | none | `dummySeed.invalid.` |
| Fixed seeds | `chainparams_seed_main` | `chainparams_seed_test` | none | none |
| Spork address | `Xgtyuk76vhuFW2iT7UAiHgNdWXCf3J34wh` | `yjPtiKh2uwk3bDutTEA2q9mCtXyiZRWn55` | same as testnet (override `-sporkaddr`; `-minsporkkeys` no longer exists) | `yj949n1UH6fDhw6HtVE5VMj2iSTaSWBMcW` (privkey `cP4EKFyJsHT39LDqgdcB43Y3YXjNyjb5Fuas1GQSeAtjnZWmZEQK` in source) |
| CoinJoin min/max participants | 3 / 20 | 2 / 20 | 2 / 20 | 2 / 20 |
| `nFulfilledRequestExpireTime` | 3600 s | 300 s | 300 s | 300 s |
| `nCreditPoolPeriodBlocks` | 576 | 576 | 576 | 100 |
| `fRequireRoutableExternalIP` / `fAllowMultipleAddressesFromGroup` | true / false | true / false | true / true | false / true |
| `nLLMQConnectionRetryTimeout` | 60 | 60 | 60 | 1 |
| minimum chain work / assumevalid | `…ba7225adc3153945d14d` / `00000000000000119fe4…b492` (height 2487500) | `…036c8f738da818d2` / `000000541a23f9db…0239` (height 1400000) | 0 | 0 |
| checkpoints | 40 (1500 … 2487500) | none | `{1: devnetGenesis}` | none |
| assumeutxo data | empty | empty | – | heights 110 and 200 (with `EvoSnapshotHash` field — Dash's assumeutxo carries an evo snapshot hash) |
| prune-after height | 100000 | 1000 | 1000 | 1000 (100 with `-fastprune`) |

### C.2 Economic / governance parameters

| param | mainnet | testnet | devnet | regtest |
|---|---|---|---|---|
| `nSubsidyHalvingInterval` (−1/14 per period) | 210240 | 210240 | 210240 | 150 |
| MN payments start / increase block / increase period | 100000 / 158000 / 17280 | 4010 / 4030 / 10 | 4010 / 4030 / 10 | 240 / 350 / 10 |
| Budget payments start / cycle / window | 328008 / 16616 / 100 | 4100 / 50 / 10 | 4100 / 50 / 10 | 1000 / 50 / 10 |
| Superblock start block / hash | 614820 / `0000000000020cb27c7e…8aa3` | 4200 / none | 4200 / none | 1500 / none |
| Superblock cycle / maturity window | 16616 / 1662 | 24 / 8 | 24 / 8 | 20 / 10 |
| Governance min quorum / filter elements | 10 / 20000 | 1 / 500 | 1 / 500 | 1 / 100 |
| MN minimum confirmations | 15 | 1 | 1 | 1 |
| InstantSend conf. required / keep lock (legacy) | 6 / 24 | 2 / 6 | 2 / 6 | 2 / 6 |
| Governance proposal collateral fee (`GOVERNANCE_PROPOSAL_FEE_TX`) / confirmations | 1 DASH / 6 on all networks (`src/governance/object.h:87-88`; the old 5-DASH pre-DIP0024 fee is gone); triggers have no fee | | | |

Block subsidy (`GetBlockSubsidyHelper`, `src/validation.cpp:1810`): **uses the previous block's height and nBits**. Base subsidy is difficulty-dependent in three eras (`<5465`: 1111/(d+1)², clamp [1,500]; `<17000` or (d≤75 and `<24000`): 11111/((d+51)/6)², clamp [25,500]; else 2222222/((d+2600)/9)², clamp [5,25]), with the mainnet `nPrevHeight<=4500` difficulty bug (`0x0000ffff/(bits&0x00ffffff)`). **From V20 the base is fixed at 5 DASH** (also on devnet from `nHighSubsidyBlocks`). Then `nSubsidy -= nSubsidy/14` for each full halving interval ≤ prevHeight; devnet multiplies by `nHighSubsidyFactor` for the first `nHighSubsidyBlocks`. The treasury (superblock) part is 10% of subsidy after `nBudgetPaymentsStartBlock`, **20% from V20**. MN share (`GetMasternodePayment`, `src/masternode/payments.cpp:62`): 20% → 50% by 2015 step schedule; from BRR (`realloc`) a superblock-cycle-aligned schedule to 60%; from V20 (`MnRewardEra::CreditPool`/`EvoReward`) 75% of (subsidy-after-treasury + fees); from MN_RR, `PlatformShare` = 37.5% of the MN reward computed on the **subsidy only** (no fees) is paid to an `OP_RETURN` output and credited to the credit pool instead of the MN (see A.3 §9.3).

### C.3 Buried deployments (`Consensus::BuriedDeployment`, names from `DeploymentName`)

| deployment (`getdeploymentinfo` name) | what it turns on | mainnet height | testnet height | devnet | regtest default (override via `-testactivationheight=name@h`) |
|---|---|---|---|---|---|
| `bip34` (HEIGHTINCB) | height in coinbase | 951 (hash `000001f35e70…8012`) | 76 | 1 | 1 (`bip34`) |
| `bip66` (DERSIG) | strict DER | 245817 | 2075 | 1 | 1 (`dersig`) |
| `bip65` (CLTV) | OP_CHECKLOCKTIMEVERIFY | 619382 | 2431 | 1 | 1 (`cltv`) |
| `csv` | BIP68/112/113 | 622944 | 8064 | 1 | 1 (`csv`) |
| `dip0001` | 2 MB blocks, lower fees ("DIP0001 activation"), `MaxBlockSize(true)` | 782208 | 5500 | 2 | 1 (`dip0001`) |
| `bip147` | NULLDUMMY | 939456 | 4300 | 1 | 0 (`bip147`) |
| `dip0003` | DIP2 special txes (tx v3 + nType/payload) & DIP3 deterministic MN list; enforcement (MN payments from DML) at `DIP0003EnforcementHeight` | 1028160, enforced 1047200 (hash `000000000000002d1734…7f81`) | 7000 / 7300 | 2 / 2 | 432 / 500 (`-dip3params=<act>:<enf>`) |
| `dip0008` | ChainLocks (LLMQ-based) and LLMQ-signing of blocks | 1088640 | 78800 | 2 | 1 (`dip0008`) |
| `realloc` (BRR) | block-reward reallocation schedule MN 50→60% | 1374912 | 387500 | 2 | 1 (`brr`) |
| `dip0020` | historically: re-enabled DIP-0020 opcodes; that script flag (`SCRIPT_ENABLE_DIP0020_OPCODES`, bit 15) is now retired and the opcodes are unconditionally enabled (`src/script/interpreter.h:101-105`). The height still gates enabling LLMQ_100_67 Platform quorums (DIP0021) (`validation.cpp:6692`) | 1516032 | 414100 | 2 | 1 (no override name) |
| `dip0024` | quorum rotation (LLMQ_60_75 rotating IS quorums, `qrinfo`), deterministic ISDLOCK, lower proposal fee | 1737792; first DIP24 quorum mined 1738698 (`DIP0024QuorumsHeight`) | 769700 / 770730 | 2 / 2 | 1 / 1 (`dip0024`) |
| `v19` | **Basic BLS scheme** (switch from legacy), EvoNodes (HPMN, 4000 DASH), ProTx v2 payloads, MN list diff v2 | 1899072 | 850100 | 2 | 1 (`v19`) |
| `v20` | EHF/MNHF signalling (MnHfTx), credit pool + asset lock/unlock (Platform withdrawals), CbTx v3 (bestCL + creditPoolBalance), fixed 5-DASH subsidy base, 20% treasury, LLMQ randomness beacon | 1987776 | 905100 | 2 | = DIP0003Height (432) (`v20`) |
| `mn_rr` | MN Reward Reallocation: platform share of MN reward → credit pool; EvoNode payout changes | 2128896 | 1066900 | 2 | = V20Height (`mn_rr`) |
| `withdrawals` | quorum-selection fix and higher asset-unlock limits | 2201472 | 1148500 | 2 | 600 (not overridable via `-testactivationheight`) |

`MinBIP9WarningHeight` = withdrawals height + 2016 (mainnet 2203488). Regtest asserts `V20Height >= DIP0003Height` and `MN_RRHeight >= V20Height`.

### C.4 Version-bits (BIP9-derived, Dash-extended) deployments (`Consensus::DeploymentPos`)

Dash's `BIP9Deployment` adds: per-deployment `nWindowSize`, `nThresholdStart`, `nThresholdMin`, `nFalloffCoeff` (**dynamic threshold**: `Threshold(nAttempt) = max(nThresholdMin, nThresholdStart - nAttempt*nAttempt*Period/100/nFalloffCoeff)` where `nAttempt` counts completed STARTED periods; falls back to `nRuleChangeActivationThreshold` if `nThresholdStart==0`, and to `nThresholdStart` if min or falloff is 0 — `src/versionbits.cpp:256-266`) and `useEHF` (**masternode hard-fork signal first**: `SignalHeight()` returns the height at which an LLMQ-signed MNHF signal for that bit was mined in a `TRANSACTION_MNHF_SIGNAL` (from `AbstractEHFManager::GetSignalsStage`), or INT_MAX if none, and miner-signal counting only starts after it; `CChainParams::IsValidMNActivation` validates the bit/time window — `src/versionbits.cpp:237-251`, `src/chainparams.cpp:111`). `min_activation_height` is the BIP341-style delay.

| deployment | bit | mainnet | testnet | devnet | regtest | notes |
|---|---|---|---|---|---|---|
| `testdummy` | 28 | NEVER_ACTIVE | NEVER_ACTIVE | NEVER_ACTIVE | start 0, no timeout | gbt_force |
| `v24` | 13 | **NEVER_ACTIVE (TODO)**, window 4032, thresholdStart 3226 (80%), thresholdMin 2420 (60%), falloff 5, useEHF | start 1790208000 (2026-09-24), window 100, 80/60, falloff 5, useEHF | start 1751328000 (2025-07-01), window 120, 96/72, falloff 5, useEHF | start 0, window 250, 200/150, falloff 5, useEHF | gbt_force. Gates: doubled asset-unlock (withdrawal) limit (`creditpool.cpp:210`), extended addresses / `netInfo` in ProTx (`providertx_service.cpp:363`), **shared-collateral masternodes** (ProRegTx v3 share table, ProDisTx=10, ProUpShareTx=11, ProUpSharedRegTx=12; covenant checks `CheckSharedCollateralSpends`/`CheckSharedCollateralTemplateOutputs` apply to *every* tx, `validation.cpp:1010`), asset-unlock stable txid (`assetlocktx.h:81`), CoinJoin changes (`coinjoin.cpp:144,260`), IS signing changes (`instantsend/signing.cpp:214`), miner/CbTx changes (`node/miner.cpp`) |

Regtest overrides: `-vbparams=<deployment>:<start>:<end>[:<min_activation_height>[:<window>:<threshold>[:<thresholdmin>:<falloffcoeff>:<useehf>]]]` (3, 4, 6 or 9 fields).

Global defaults: `nRuleChangeActivationThreshold`/`nMinerConfirmationWindow` mainnet 1815/2016 (90%), testnet & devnet 1512/2016 (75%), regtest 108/144.

### C.5 PoW parameters

| | mainnet | testnet | devnet | regtest |
|---|---|---|---|---|
| powLimit | `00000fff…` (~2^236) | `00000fff…` | `7fff…` | `7fff…` |
| target spacing / timespan | 150 s / 86400 s | same | 150 s (override `-powtargetspacing`) / 86400 | same |
| min-difficulty blocks allowed / no-retarget | false / false | true / false | true / false (+ `-minimumdifficultyblocks`) | true / **true** |
| KGW height / DGW height | 15200 / 34140 (BTC-style retarget before KGW) | 4002 / 4002 (no KGW) | 4001 / 4001 | 15200 / 34140 |

Block hash = X11 of the 80-byte header. Note the comment in `params.h`: observed mainnet spacing ≈2.626 min due to the historical DGW off-by-one — must be reproduced exactly.

Block size: `MaxBlockSize(fDIP0001Active)` = 2 MB after DIP0001 (1 MB before); `MaxBlockSigOps` = size/50 (`src/consensus/consensus.h:12-19`). Special-tx version `CTransaction::SPECIAL_VERSION = 3`.

### C.6 LLMQ types (`src/llmq/params.h`, `available_llmqs`, 14 entries; `MAX_LLMQ_SIZE` caps test overrides)

| id | name | rot. | size | minSize | threshold | dkgInterval | phaseBlocks | mining window | badVotes | signingActive | keepOldConn | keepOldKeys | recoveryMembers |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 1 | llmq_50_60 | no | 50 | 40 | 30 | 24 | 2 | 10–18 | 40 | 24 | 25 | 48 | 25 |
| 2 | llmq_400_60 | no | 400 | 300 | 240 | 288 | 4 | 20–28 | 300 | 4 | 5 | 8 | 100 |
| 3 | llmq_400_85 | no | 400 | 350 | 340 | 576 | 4 | 20–48 | 300 | 4 | 5 | 8 | 100 |
| 4 | llmq_100_67 | no | 100 | 80 | 67 | 24 | 2 | 10–18 | 80 | 24 | 25 | 1440 | 50 |
| 5 | llmq_60_75 | **yes** | 60 | 50 | 45 | 288 | 2 | 42–50 | 48 | 32 | 64 | 64 | 25 |
| 6 | llmq_25_67 | no | 25 | 22 | 17 | 24 | 2 | 10–18 | 22 | 24 | 25 | 1440 | 12 |
| 100 | llmq_test | no | 3 | 2 | 2 | 24 | 2 | 10–18 | 2 | 2 | 3 | 4 | 3 |
| 101 | llmq_devnet | no | 12 | 7 | 6 | 24 | 2 | 10–18 | 7 | 4 | 5 | 8 | 6 |
| 102 | llmq_test_v17 | no | 3 | 2 | 2 | 24 | 2 | 10–18 | 2 | 2 | 3 | 4 | 3 |
| 103 | llmq_test_dip0024 | **yes** | 4 | 4 | 3 | 24 | 2 | 12–20 | 2 | 2 | 4 | 4 | 3 |
| 104 | llmq_test_instantsend | no | 3 | 2 | 2 | 24 | 2 | 10–18 | 2 | 2 | 3 | 4 | 3 |
| 105 | llmq_devnet_dip0024 | **yes** | 8 | 6 | 4 | 48 | 2 | 12–20 | 7 | 2 | 4 | 4 | 4 |
| 106 | llmq_test_platform | no | 3 | 2 | 2 | 24 | 2 | 10–18 | 2 | 2 | 4 | 1440 | 3 |
| 107 | llmq_devnet_platform | no | 12 | 9 | 8 | 24 | 2 | 10–18 | 7 | 4 | 5 | 1440 | 6 |

Per-network enabled types and role assignment:

| role | mainnet | testnet | devnet (override flag) | regtest (override flag) |
|---|---|---|---|---|
| enabled types (enable-height rules in `ChainstateManager::IsQuorumTypeEnabled`, `validation.cpp:6654`: 100_67 after DIP0020, rotating types after DIP0024, 25_67 after testnet height 847000 `TESTNET_LLMQ_25_67_ACTIVATION_HEIGHT`, llmq_test_v17 only when `testdummy` is active) | 50_60, 60_75, 400_60, 400_85, 100_67 | + 25_67 | 50_60, 60_75, 400_60, 400_85, 100_67, devnet, devnet_dip0024, devnet_platform | test, test_instantsend, test_v17, test_dip0024, test_platform |
| ChainLocks | 400_60 | 50_60 | llmq_devnet (`-llmqchainlocks`, must be non-rotating) | llmq_test |
| InstantSend (DIP0024, rotating) | 60_75 | 60_75 | llmq_devnet_dip0024 (`-llmqinstantsenddip0024`, must rotate) | llmq_test_dip0024 (`-llmqtestinstantsenddip0024`) |
| Platform | 100_67 | 25_67 | llmq_devnet_platform (`-llmqplatform`) | llmq_test_platform |
| MNHF (EHF signals) | 400_85 | 50_60 | llmq_devnet (`-llmqmnhf`) | llmq_test |
| size/threshold overrides | – | – | `-llmqdevnetparams=<size>:<threshold>` (llmq_devnet) | `-llmqtestparams`, `-llmqtestinstantsendparams`, `-llmqtestplatformparams` = `<size>:<threshold>` |

(Legacy non-rotating InstantSend `llmqTypeInstantSend` no longer exists in params; only the DIP0024 type remains. Asset-unlock quorum selection uses the Platform type — see A.evo.)

### C.7 Other devnet/regtest knobs that change consensus

`-devnet=<name>` (name goes into devnet genesis coinbase, so it changes all hashes), `-highsubsidyblocks`, `-highsubsidyfactor`, `-minimumdifficultyblocks`, `-powtargetspacing` (devnet); `-budgetparams=<mnStart>:<budgetStart>:<superblockStart>`, `-dip3params`, `-testactivationheight`, `-vbparams`, `-llmqtest*params` (regtest); `-sporkaddr`, `-sporkkey` (devnet/regtest spork control). A Rust node must accept the same flags with the same parse rules to be usable in Dash's functional-test harness and devnets.

---

## D. User-story catalog (target parity surface)

Conventions: IDs are stable handles for PRD/PLAN tracing. "Implemented by" lists the RPCs (see R2b) / options / subsystems in Dash Core that realise the story today. "UX" notes where Dash Core exposes it only through Qt (the Rust node's desktop UI is built separately, so those stories need a backend API but not identical UX). Priority hint: **P0** = required for a node to be a drop-in dashd replacement on the network; **P1** = required for user-story parity; **P2** = niche/debug.

### D.1 Node operator (US-NODE)

| ID | Story | Implemented by | Pri |
|---|---|---|---|
| US-NODE-01 | Run a full validating node on mainnet/testnet/devnet/regtest, syncing headers+blocks from genesis with assumevalid/checkpoints | `dashd`, `-chain/-testnet/-devnet/-regtest`, validation, net_processing, `getblockchaininfo`, `getblockcount` | P0 |
| US-NODE-02 | Know when the node (and the Dash "special" layers) are fully synced | `mnsync status`, `getblockchaininfo.verificationprogress/initialblockdownload`, masternode sync (`MASTERNODE_SYNC_BLOCKCHAIN → GOVERNANCE → FINISHED`) | P0 |
| US-NODE-03 | Configure via `dash.conf` (network sections, includeconf) and `settings.json`, CLI flags, with identical option names/defaults | ArgsManager, `-conf`, `-datadir`, `-includeconf`, `-settings` | P0 |
| US-NODE-04 | Run headless as a daemon, start/stop cleanly, survive crashes (flush, `-dbcache`, `-reindex`, `-reindex-chainstate`, `-loadblock`) | `dashd -daemon`, `stop`, `uptime`, `-pid`, `-dbcache`, `-reindex*`, `verifychain`, `-checkblocks/-checklevel` | P0 |
| US-NODE-05 | Prune block storage to save disk (losing txindex/addressindex ability) | `-prune`, `pruneblockchain` | P1 |
| US-NODE-06 | Manage peers: add/remove/disconnect, ban/unban, view peer info incl. masternode/mnauth status, connection types | `addnode`, `disconnectnode`, `getaddednodeinfo`, `getpeerinfo`, `setban`/`listbanned`/`clearbanned`, `getconnectioncount`, `getnodeaddresses`, `ping`, `setnetworkactive` | P0 |
| US-NODE-07 | Run over Tor / I2P / CJDNS, publish an onion service, restrict networks | `-proxy`, `-onion`, `-listenonion`, `-torcontrol`, `-i2psam`, `-onlynet`, `-cjdnsreachable`, torcontrol.cpp, i2p.cpp | P1 |
| US-NODE-08 | Use NAT-PMP/UPnP port mapping, `-externalip`, `-discover`, `-bind`/`-whitebind`, permissions | mapport, net_permissions, `-whitelist` | P1 |
| US-NODE-09 | Limit bandwidth and resources (`-maxuploadtarget`, `-maxconnections`, `-maxmempool`, `-par`, `-rpcthreads`) | net, txmempool, init | P1 |
| US-NODE-10 | Inspect network & traffic stats | `getnetworkinfo`, `getnettotals`, `dash-cli -netinfo`, `-getinfo`, `getmemoryinfo` | P1 |
| US-NODE-11 | Get alerts and hooks: `-blocknotify`, `-chainlocknotify`, `-instantsendnotify`, `-alertnotify`, `-startupnotify`, `-shutdownnotify` | init.cpp notifications | P1 |
| US-NODE-12 | Change logging categories at runtime; read `debug.log` with identical categories (incl. Dash ones: chainlocks, coinjoin, gobject, instantsend, llmq, llmq-dkg, llmq-sigs, mnpayments, mnsync, spork, netconn, ehf, creditpool…) | `logging`, `debug`, `-debug`, `-debugexclude`, `-logtimestamps`, … | P1 |
| US-NODE-13 | Observe sporks and their effect | `spork show/active`, spork.cpp | P0 |
| US-NODE-14 | See active/upcoming consensus deployments (buried + EHF/BIP9) | `getdeploymentinfo`, `getblockchaininfo` | P0 |
| US-NODE-15 | Export metrics to statsd | `-statshost/-statsport/-statsprefix/-statsperiod…`, src/stats | P2 |
| US-NODE-16 | Bootstrap from a UTXO snapshot (assumeutxo; Dash's `m_assumeutxo_data` entries carry an `EvoSnapshotHash`, regtest only so far; there is no `loadtxoutset` RPC yet, only `dumptxoutset` (hidden)) | `dumptxoutset`, `src/evo/snapshot.*` | P2 |
| US-NODE-17 | Recover from forks: invalidate/reconsider/precious block; see chaintips incl. ChainLock-conflicting forks | `invalidateblock`, `reconsiderblock`, `preciousblock`, `getchaintips` | P1 |
| US-NODE-18 | Migrate an existing Dash Core datadir (blocks, chainstate, wallets, peers, bans, governance cache) into the new node | §E import matrix | P0 (program goal) |

### D.2 Wallet user (US-WAL)

| ID | Story | Implemented by | Pri |
|---|---|---|---|
| US-WAL-01 | Create/load/unload/list/backup/restore wallets (multiwallet) | `createwallet`, `loadwallet`, `unloadwallet`, `listwallets`, `listwalletdir`, `backupwallet`, `restorewallet`, `-wallet`, `-walletdir` | P0 |
| US-WAL-02 | HD wallet from a BIP39 mnemonic (+passphrase), BIP44 `m/44'/5'/0'`; view/back up mnemonic | `createwallet` (descriptors default) / `upgradetohd`, `dumphdinfo`, `-mnemonic`, `-mnemonicpassphrase`, `-usehd`; `sethdseed` | P0 |
| US-WAL-03 | Receive: new addresses, labels, address info, payment requests (Qt) | `getnewaddress`, `getrawchangeaddress`, `setlabel`, `getaddressinfo`, `getaddressesbylabel`, `listlabels`, `keypoolrefill`, `newkeypool` | P0 |
| US-WAL-04 | See balances (trusted/pending/immature/CoinJoin-mixed/IS-locked) and history | `getbalance`, `getbalances`, `getunconfirmedbalance`, `listtransactions`, `listsinceblock`, `gettransaction` (incl. `instantlock`, `chainlock`), `listreceivedby*`, `getreceivedby*`, `listaddressbalances`, `listaddressgroupings` | P0 |
| US-WAL-05 | Send DASH with automatic fee, coin control, InstantSend-locked confirmations, subtract-fee, send-all | `sendtoaddress`, `sendmany`, `send`, `sendall`, `settxfee`, `-paytxfee/-mintxfee/-fallbackfee`, `lockunspent`, `listunspent`, `listlockunspent` | P0 |
| US-WAL-06 | Encrypt/lock/unlock wallet; change passphrase; unlock for mixing only | `encryptwallet`, `walletpassphrase` (mixingonly flag), `walletlock`, `walletpassphrasechange` | P0 |
| US-WAL-07 | Import/export keys, watch-only addresses, descriptors, dumps; import Electrum (Dash Electrum) exports | `importprivkey`, `importpubkey`, `importaddress`, `importmulti`, `importdescriptors`, `listdescriptors`, `dumpprivkey`, `dumpwallet`, `importwallet`, `importelectrumwallet`, `importprunedfunds`, `removeprunedfunds` | P1 |
| US-WAL-08 | Rescan chain / abort rescan; abandon stuck tx; wipe tx history | `rescanblockchain`, `abortrescan`, `abandontransaction`, `wipewallettxes` | P1 |
| US-WAL-09 | Multisig and PSBT workflows; hardware/external signer | `addmultisigaddress`, `createmultisig`, `walletcreatefundedpsbt`, `walletprocesspsbt`, `*psbt`, `enumeratesigners`, `walletdisplayaddress`, `-signer` | P1 |
| US-WAL-10 | Sign/verify messages | `signmessage`, `verifymessage`, `signmessagewithprivkey` | P1 |
| US-WAL-11 | Upgrade / migrate legacy (BDB) wallets to descriptor (SQLite) wallets; offline wallet tool | `upgradewallet`, `migratewallet`, `dash-wallet create/info/dump/createfromdump/salvage` | P0 (import goal) |
| US-WAL-12 | Get notified on wallet tx (`-walletnotify`) and via ZMQ | `-walletnotify`, ZMQ | P1 |
| US-WAL-13 | Simulate a tx's balance effect before signing | `simulaterawtransaction` | P2 |
| US-WAL-14 | Set wallet flags (avoid_reuse etc.) | `setwalletflag` | P2 |

### D.3 Masternode owner / operator (US-MN)

| ID | Story | Implemented by | Pri |
|---|---|---|---|
| US-MN-01 | Find eligible 1000-DASH (regular) / 4000-DASH (Evo) collateral outputs | `masternode outputs` | P0 |
| US-MN-02 | Generate operator BLS keys (basic scheme) | `bls generate`, `bls fromsecret` | P0 |
| US-MN-03 | Register a masternode (ProRegTx) — collateral in wallet, funded in-tx, or external/hardware collateral via prepare/sign/submit | `protx register`, `protx register_fund`, `protx register_prepare` + `signmessage` (external) + `protx register_submit` | P0 |
| US-MN-04 | Run the masternode: configure operator key and external IP, get `READY` state, answer mnauth, participate in quorums | `-masternodeblsprivkey`, `-externalip`, active masternode manager, `masternode status` | P0 |
| US-MN-05 | Update service (IP/port; extended netInfo after V24) as operator | `protx update_service` | P0 |
| US-MN-06 | Change operator/voting keys or payout as owner | `protx update_registrar` | P0 |
| US-MN-07 | Revoke operator (operator-initiated, with reason) | `protx revoke` | P1 |
| US-MN-08 | Get paid: MN appears in payment queue, receives MN reward each cycle; operator reward split | DML payee selection, `masternode payments`, `masternode winners`, `protx info` (operatorReward, operatorPayoutAddress) | P0 |
| US-MN-09 | Monitor PoSe score/ban and revive by ProUpServTx | `protx info`, `masternode list`/`masternodelist`, `protx list` (registered/valid/wallet), `masternode count` | P0 |
| US-MN-10 | Inspect DML at heights / diffs (also used by SPV/Platform) | `protx diff`, `protx listdiff`, `masternode list` modes | P1 |
| US-MN-11 | Participate in DKG and signing for each LLMQ type; debug DKG | quorum DKG engine, `quorum dkgstatus`, `quorum dkginfo`, `quorum memberof`, `quorum list/info` | P0 |
| US-MN-12 | Serve quorum data recovery to peers / recover own shares | `-llmq-data-recovery`, `-llmq-qvvec-sync`, `quorum getdata` | P1 |
| US-MN-13 | Run CoinJoin mixing server (masternodes automatically act as mixing servers) | CoinJoin server (`dsa/dsq/dsi/dsf/dss/dsc`) | P0 |
| US-MN-14 | Sign ChainLocks and InstantSend locks as quorum member | chainlock/instantsend signers, sig shares | P0 |
| US-MN-15 | Vote on governance as voting-key holder (see US-GOV) and as operator-key holder for triggers | `gobject vote-many/vote-alias`, trigger voting in governance signer | P0 |
| US-MN-16 | Use legacy-BLS keys on old registrations (deprecated) | `protx *_legacy` (`-deprecatedrpc=legacy_mn`) | P2 |
| US-MN-17 | Watch the MN as an owner from a non-MN wallet (owner/voting/payout key presence) | `protx list wallet`, `protx info` (wallet section) | P1 |

### D.4 Shared / collateral-split masternode participant (US-SHR) — V24

New in v24 (present in `v24.0.0-rc.2`). The V24 EHF deployment start time on testnet is 2026-09-24; activation still needs an MNHF signal plus the miner threshold. Mainnet start is still `NEVER_ACTIVE` (TODO).

| ID | Story | Implemented by | Pri |
|---|---|---|---|
| US-SHR-01 | Several parties co-fund one masternode collateral (≥100 DASH per share), each with immutable refund address and owner key, optional reward address | `protx shared_register_prepare` (ProRegTx v3 + share table), `protx shared_sign`, `protx shared_combine`, `signrawtransactionwithwallet`, `sendrawtransaction` | P1 (gated by V24) |
| US-SHR-02 | Each participant verifies its share terms before signing funding inputs | `protx shared_sign` (`terms`, `warning`), `decoderawtransaction` (`proRegTx` JSON) | P1 |
| US-SHR-03 | Receive per-share rewards to own reward script; change own reward address | `protx shared_update_share` (ProUpShareTx, type 11) | P1 |
| US-SHR-04 | Jointly change operator/voting key | `protx shared_update_registrar_prepare` → `shared_sign` → `shared_combine` (ProUpSharedRegTx, type 12) | P1 |
| US-SHR-05 | Exit: unanimous zero-penalty dissolution, or unilateral dissolution (early-period penalty redistributed pro-rata); pre-sign a standby dissolution for trustless recovery | `protx shared_dissolve_prepare` / `protx shared_dissolve` (ProDisTx, type 10, fee ≤ 1,000,000 duffs, earlyPeriodBlocks ≤ 420480) | P1 |

### D.5 Governance participant / proposal author (US-GOV)

| ID | Story | Implemented by | Pri |
|---|---|---|---|
| US-GOV-01 | Browse proposals/triggers and their vote tallies; see governance params and next superblock budget | `gobject list/diff/get/count/getcurrentvotes`, `getgovernanceinfo`, `getsuperblockbudget` | P0 |
| US-GOV-02 | Validate and author a proposal JSON (name, url, payment address, amount, epochs) | `gobject check`, `gobject deserialize` | P1 |
| US-GOV-03 | Pay the 1-DASH proposal collateral (OP_RETURN of object hash) and submit after 6 confirmations | `gobject prepare`, `gobject list-prepared`, `gobject submit` | P0 |
| US-GOV-04 | Vote funding/valid/delete/endorsed yes/no/abstain with one or all owned MN voting keys | `gobject vote-many`, `gobject vote-alias` | P0 |
| US-GOV-05 | Vote with an externally produced signature (hardware/offline voting key) | `voteraw` | P1 |
| US-GOV-06 | Get notified of new objects/votes | ZMQ `pubhashgovernancevote`, `pubhashgovernanceobject`, `pubrawgovernancevote`, `pubrawgovernanceobject` | P1 |
| US-GOV-07 | (Masternodes) automatically create, sign and vote superblock triggers; miners include superblock payments | governance signer / CSuperblockManager, `getblocktemplate.superblock[]` | P0 |

### D.6 CoinJoin user (US-CJ)

| ID | Story | Implemented by | Pri |
|---|---|---|---|
| US-CJ-01 | Start/stop/reset mixing in a wallet; see progress and sessions | `coinjoin start/stop/reset/status`, `getcoinjoininfo` | P1 |
| US-CJ-02 | Configure rounds, target amount, sessions, multisession, denom goals/hardcap | `setcoinjoinrounds`, `setcoinjoinamount`, `-coinjoinrounds`, `-coinjoinamount`, `-coinjoinsessions`, `-coinjoinmultisession`, `-coinjoindenomsgoal`, `-coinjoindenomshardcap`, `-enablecoinjoin`, `-coinjoinautostart` | P1 |
| US-CJ-03 | Spend only fully-mixed funds (private send) and see mixed balance | coin types (`ONLY_FULLY_MIXED`) in `send`/`sendtoaddress` `use_cj`, `getbalances.mine.coinjoin`, `listunspent` (coinjoin_rounds) | P1 |
| US-CJ-04 | Manage the per-wallet CoinJoin salt (affects round derivation) | `coinjoinsalt generate/get/set` | P2 |
| US-CJ-05 | Keep keypool large enough for mixing; unlock wallet "for mixing only" | `walletpassphrase … mixingonly`, `-keypool` | P1 |

### D.7 Miner / pool (US-MINE)

| ID | Story | Implemented by | Pri |
|---|---|---|---|
| US-MINE-01 | Get a block template with correct CbTx payload, MN payment(s), superblock payments, quorum commitments, MNHF signals, asset unlocks, credit-pool balance | `getblocktemplate` (`coinbase_payload`, `masternode`, `superblock`, `masternode_payments_started/enforced`, `superblocks_started/enabled`) | P0 |
| US-MINE-02 | Submit blocks/headers; propose blocks | `submitblock`, `submitheader`, GBT `proposal` mode | P0 |
| US-MINE-03 | See mining info / network hashrate; prioritise txs | `getmininginfo`, `getnetworkhashps`, `prioritisetransaction`, `getprioritisedtransactions`, `-blockmaxsize`, `-blockmintxfee` | P1 |
| US-MINE-04 | Signal version bits / EHF deployments (miners signal bit 13 for v24 after MNHF) | block version computation, `getblocktemplate.rules/vbavailable` | P0 |
| US-MINE-05 | Regtest mining for tests | `generatetoaddress`, `generatetodescriptor`, `generateblock`, `dash-cli -generate` | P0 (tests) |

### D.8 Exchange / integrator / explorer (US-INT)

| ID | Story | Implemented by | Pri |
|---|---|---|---|
| US-INT-01 | Look up any tx (txindex) incl. IS/CL status; batch lookup | `-txindex` (**default on** in Dash, `DEFAULT_TXINDEX=true`; governance validation refuses to start without it outside regtest unless `-disablegovernance`, `init.cpp:1443`), `getrawtransaction`, `getrawtransactionmulti`, `gettxchainlocks`, `getislocks` | P0 |
| US-INT-02 | Address-centric queries (balance, utxos, deltas, txids, mempool) | `-addressindex`, `getaddressbalance/utxos/deltas/txids/mempool` | P0 (explorers, Insight) |
| US-INT-03 | Spent-output and timestamp lookups | `-spentindex` (`getspentinfo`), `-timestampindex` (`getblockhashes`) | P1 |
| US-INT-04 | Accept deposits on InstantSend lock instead of confirmations; treat ChainLocked blocks as final | `gettransaction.instantlock/chainlock`, ZMQ `hashtxlock`/`rawtxlock`/`rawtxlocksig`/`hashchainlock`/`rawchainlock(sig)`, `-instantsendnotify`, `-chainlocknotify` | P0 |
| US-INT-05 | Stream events via ZMQ: `pubhashblock`, `pubrawblock`, `pubhashtx`, `pubrawtx`, `pubsequence`, `pubhashtxlock`, `pubrawtxlock`, `pubrawtxlocksig`, `pubhashchainlock`, `pubrawchainlock`, `pubrawchainlocksig`, `pubhashgovernancevote`, `pubrawgovernancevote`, `pubhashgovernanceobject`, `pubrawgovernanceobject`, `pubhashinstantsenddoublespend`, `pubrawinstantsenddoublespend`, `pubhashrecoveredsig`, `pubrawrecoveredsig` (19 topics) | src/zmq, `getzmqnotifications` | P0 |
| US-INT-06 | REST read-only access (blocks, headers, tx, utxos, mempool, chaininfo, blockhashbyheight, blockfilter) | `-rest`, src/rest.cpp | P1 |
| US-INT-07 | Compact block filters (BIP157/158) for light clients | `-blockfilterindex`, `-peerblockfilters`, `getblockfilter`, `scanblocks` | P1 |
| US-INT-08 | UTXO-set stats and scans | `gettxoutsetinfo` (+`-coinstatsindex`), `scantxoutset`, `gettxout` | P1 |
| US-INT-09 | Fee estimation | `estimatesmartfee`, `estimaterawfee`, `fee_estimates.dat` | P1 |
| US-INT-10 | Broadcast & test transactions | `sendrawtransaction`, `testmempoolaccept`, `submitpackage` (regtest), mempool RPCs | P0 |
| US-INT-11 | Merkle proofs / SPV-style proofs for txs | `gettxoutproof`, `verifytxoutproof`, `getmerkleblocks` | P1 |
| US-INT-12 | Inspect special txs in blocks (ProTx, CbTx, QC, asset lock/unlock) | `getspecialtxes`, `getblock` verbosity 2 (decoded payloads), `decoderawtransaction` | P1 |

### D.9 Dash Platform / Evonode operator (US-PLAT)

| ID | Story | Implemented by | Pri |
|---|---|---|---|
| US-PLAT-01 | Register/update an EvoNode with platformNodeID, platformP2PPort, platformHTTPPort (4000 DASH) | `protx register_evo`, `register_fund_evo`, `register_prepare_evo`, `protx update_service_evo` | P0 |
| US-PLAT-02 | Platform (Tenderdash/Drive) queries Core for quorums, MN lists, chainlocks over RPC/ZMQ | `quorum listextended`, `quorum info`, `quorum selectquorum`, `protx diff/listdiff`, `masternodelist`, `getbestchainlock`, `getblockcount`, `getblockheader`, `getrawtransaction`, `-rpcwhitelist` restricted user, ZMQ `rawchainlocksig`, `rawrecoveredsig`, `hashrecoveredsig` | P0 |
| US-PLAT-03 | Platform requests threshold signatures from Platform quorums and verifies signatures | `quorum platformsign`, `quorum sign`, `quorum verify`, `quorum getrecsig`, `quorum hasrecsig`, `quorum isconflicting` | P0 |
| US-PLAT-04 | Credits: users lock DASH into Platform (AssetLockTx); Platform withdrawals unlock it (AssetUnlockTx signed by Platform quorum) within credit-pool limits | `getcreditpoolinfo`, `getassetunlockstatuses`, asset lock/unlock validation, credit pool | P0 |
| US-PLAT-05 | Platform/light clients verify quorum chain proofs | `getquorumproofchain` / `verifyquorumproofchain` (DASHNC02), `quorum rotationinfo`, `verifychainlock`, `verifyislock`, `getchainlockbyheight` | P1 |
| US-PLAT-06 | Platform reward share flows into credit pool (MN_RR) and Evonodes get paid accordingly | payments, credit pool | P0 |
| US-PLAT-07 | Platform-originated bans of misbehaving Evonodes, relayed as `platformban` P2P messages (`src/protocol.cpp:93`) | net_processing / evo (see A.6) | P1 |

### D.10 Developer on regtest / devnet (US-DEV)

| ID | Story | Implemented by | Pri |
|---|---|---|---|
| US-DEV-01 | Spin up regtest with instant blocks, masternodes, quorums, ChainLocks, IS in functional tests | `-regtest`, `generate*`, DashTestFramework, `-llmqtest*params`, `-dip3params`, `-budgetparams`, `-testactivationheight`, `-vbparams` | P0 |
| US-DEV-02 | Launch a named devnet with custom subsidy/difficulty/LLMQ types/spork keys | `-devnet=<name>`, `-highsubsidyblocks/-factor`, `-minimumdifficultyblocks`, `-powtargetspacing`, `-llmqchainlocks/-llmqinstantsenddip0024/-llmqplatform/-llmqmnhf/-llmqdevnetparams`, `-sporkaddr`, `-sporkkey` | P1 |
| US-DEV-03 | Control time and scheduler; mock peers/messages | `setmocktime`, `mockscheduler`, `sendmsgtopeer`, `addconnection`, `addpeeraddress`, `mnauth`, `setmnthreadactive`, `quorum dkgsimerror` | P0 (tests) |
| US-DEV-04 | Flip sporks on devnet/regtest | `sporkupdate` (with `-sporkkey`) | P0 (tests) |
| US-DEV-05 | Decode/construct raw objects | `decoderawtransaction`, `decodescript`, `createrawtransaction`, `gobject deserialize`, `dash-tx`, `dash-util` | P1 |
| US-DEV-06 | Inspect internal DBs, repair evodb | `evodb verify/repair`, `getindexinfo`, `getrpcinfo`, `syncwithvalidationinterfacequeue`, `waitfor*` | P2 |

---

## E. Data formats and import

### E.summary Import matrix summary

- **Required:** `blocks/blk*.dat`, `blocks/rev*.dat`, `blocks/index/`, `chainstate/` and `evodb/`.
  - These must be imported as a set that agrees on one tip. On blocks where DIP3 is active, `CEvoDB::VerifyBestBlock` checks the evodb best block (`b_b4`) against the chainstate tip (`src/validation.cpp:2289,2528,5254`).
- **Recommended:** `llmq/quorumdb` (quorum vvec and sk-share cache) and `llmq/isdb` (InstantSend locks). Without them a node must rebuild them from the network.
- **Can be regenerated:** `indexes/*` (txindex, blockfilter, coinstats, address, spent, timestamp), `llmq/recsigdb`, `llmq/dkgdb`, and the flat files (`governance.dat`, `mncache.dat`, `netfulfilled.dat`, `sporks.dat`, `peers.dat`, `anchors.dat`, `mempool.dat`, `fee_estimates.dat`, `banlist.json`).
- **Wallets:** legacy BDB 4.8 or SQLite. The SQLite `application_id` equals the network magic read big-endian.
- **Obfuscation:** only `chainstate/` (and `chainstate_snapshot/`) use the LevelDB value XOR key. Dash has no `xor.dat` and no `-blocksxor`, so block files are plaintext.
- **Hash split:** block hashes are X11. Every checksum (undo files, flat DBs, peers.dat, wallet dump) and every txid is SHA256d.

---

### E.0 Datadir layout per network

`GetDataDirNet()` returns `<datadir>/<BaseParams().DataDir()>` (`src/chainparamsbase.cpp:39-51`).

| Network | Subdir | P2P / onion port | Message start (`pchMessageStart`) | Source |
|---|---|---|---|---|
| main | `""` (the datadir root) | 9998 / 9996 | `bf 0c 6b bd` | `chainparams.cpp:229-232` |
| testnet | `testnet3/` | 19998 / 19996 | `ce e2 ca ff` | `:424-427` |
| devnet | `devnet-<name>/` (or `devnet/` if the name is empty; `util/system.cpp:1051-1054`) | 19798 / 19796 | `e2 ca ff ce` | `:597-600` |
| regtest | `regtest/` | 19898 / 19896 | `fc c1 b7 dc` | `:832-835` |

Per-network directory contents:

```
<net>/
  blocks/            blk%05u.dat, rev%05u.dat (FlatFileSeq, flatfile.cpp:31), index/ (LevelDB)
  chainstate/        LevelDB (obfuscated); chainstate_snapshot/ while assumeutxo is active (node/utxo_snapshot.h:66)
  evodb/             LevelDB (evo/evodb.cpp:38, 64 MiB cache)
  llmq/isdb/ llmq/recsigdb/ llmq/dkgdb/ llmq/quorumdb/   LevelDB
  indexes/txindex/ indexes/addressindex/ indexes/spentindex/ indexes/timestampindex/
  indexes/blockfilter/basic/{db/, fltr%05u.dat}  indexes/coinstats/db/
  wallets/ (if the dir exists, otherwise the net dir; wallet/walletutil.cpp:24-27)
  governance.dat mncache.dat netfulfilled.dat sporks.dat
  peers.dat anchors.dat banlist.json (banlist.dat is legacy and ignored) mempool.dat fee_estimates.dat
  settings.json .cookie .lock debug.log dashd.pid onion_v3_private_key i2p_private_key
  message_capture/<addr>/msgs_{recv,sent}.dat (only with -capturemessages)
```

Other version constants:
- `CLIENT_VERSION = 10000*major + 100*minor + build`, so develop 24.1.0 gives **240100** (`clientversion.h:33`, `configure.ac:3-5`).
- `PROTOCOL_VERSION = 70242`, `MIN_PEER_PROTO_VERSION = 70221` (`version.h:14,20`).

### E.1 LevelDB wrapper (`CDBWrapper`), common to all LevelDB stores

Source: `src/dbwrapper.{h,cpp}`. The bundled LevelDB is **1.22** (`src/leveldb/include/leveldb/db.h:18-19`).

Options (`dbwrapper.cpp:113-127`):
- `compression = kNoCompression`
- bloom filter at 10 bits
- `paranoid_checks = true`
- default bytewise comparator
- `verify_checksums = true` on reads

**Keys** are the serialized value of the C++ key object: `CDataStream(SER_DISK, CLIENT_VERSION)`.
- A `uint8_t` prefix is 1 byte.
- A `std::string` prefix is CompactSize length followed by the bytes. For example `"dmn_S3"` is `06 64 6d 6e 5f 53 33`.
- `std::pair` and `std::tuple` are plain concatenation.
- Keys are **never** obfuscated.

**Values** are serialized with `SER_DISK, CLIENT_VERSION` and then XORed with `obfuscate_key`, repeating the key cyclically from offset 0 (`util::Xor` at `streams.h:28-45`; applied at `dbwrapper.h:116,209,300`).

**Obfuscation key** (`dbwrapper.cpp:130-184,232-247`):
- Record key: `std::string("\000obfuscate_key", 14)`, serialized as `0e 00 6f 62 66 75 73 63 61 74 65 5f 6b 65 79`.
- Record value: `std::vector<unsigned char>` of 8 random bytes, serialized as `08` plus 8 bytes, stored un-XORed. It is read while the in-memory key is still all zeros.
- A new key is created only when `obfuscate=true` **and** the DB is empty. Otherwise the key is all zeros, which means no obfuscation.

Which stores pass `obfuscate=true`:

| Store | obfuscate | Construction |
|---|---|---|
| `chainstate/` (`CCoinsViewDB`) | **true** | `txdb.cpp:73,85` |
| `blocks/index` (`CBlockTreeDB`) | false | `txdb.cpp:181` |
| `indexes/*` (`BaseIndex::DB`, default `f_obfuscate=false`) | false | `index/base.h:47-48`; `txindex.cpp:34`, `addressindex.cpp:22`, `spentindex.cpp:23`, `timestampindex.cpp:16`, `blockfilterindex.cpp:117`, `coinstatsindex.cpp:111-114` |
| `evodb`, `llmq/*` (`util::MakeDbWrapper`) | false | `dbwrapper.h:762-773` |

A Rust reader should always read `\0obfuscate_key` if it is present and apply it, regardless of which store it is reading.

**Complete list of `CDBWrapper` constructions** (from grep):

| Dir | Class | Cache | Location |
|---|---|---|---|
| `blocks/index` | CBlockTreeDB | configurable | txdb.cpp:181 |
| `chainstate[ _snapshot]` | CCoinsViewDB | configurable | txdb.cpp:73; validation.cpp:1951 |
| `evodb` | CEvoDB | 64 MiB | evo/evodb.cpp:38 |
| `llmq/isdb` | CInstantSendDb | 32 MiB | instantsend/db.cpp:30 (re-created on upgrade at :44) |
| `llmq/recsigdb` | CRecoveredSigsDb | 8 MiB | llmq/signing.cpp:25 |
| `llmq/dkgdb` | CDKGSessionManager | 1 MiB | llmq/dkgsessionmgr.cpp:35 |
| `llmq/quorumdb` | CQuorumManager | 1 MiB | llmq/quorumsman.cpp:39 |
| `indexes/{txindex,addressindex,spentindex,timestampindex}` | BaseIndex::DB | configurable | index/*.cpp |
| `indexes/blockfilter/basic/db` | BlockFilterIndex | configurable | blockfilterindex.cpp:114-117 |
| `indexes/coinstats/db` | CoinStatsIndex | configurable | coinstatsindex.cpp:111-114 |
| temporary `chainstate` and `indexes/*` handles | migration only | 0 | txdb.cpp:541,560,577,592 |

**VARINT** (`serialize.h:387-402`) is Bitcoin's MSB-first base-128 encoding with the "-1" offset: `tmp[len] = (n & 0x7F) | (len ? 0x80 : 0)`, then `n = (n >> 7) - 1`, and the bytes are emitted in reverse order. This is different from CompactSize.

### E.2 `blocks/blk?????.dat` and `rev?????.dat`

Code: `src/node/blockstorage.{h,cpp}`.

**Sizes:** `MAX_BLOCKFILE_SIZE = 0x8000000` (128 MiB). Pre-allocation chunks are `BLOCKFILE_CHUNK_SIZE = 16 MiB` and `UNDOFILE_CHUNK_SIZE = 1 MiB` (`blockstorage.h:39-44`). With `-tinyblk` both chunks are 64 KiB (`blockstorage.cpp:633-640`); `-fastprune` also exists. File naming is `"%s%05u.dat"` (`flatfile.cpp:31`). Pre-allocated tails are zero-filled, so a reader must stop when it hits a zero magic.

**blk record** (`WriteBlockToDisk`, `:748-770`): `MessageStart[4] || uint32 LE nSize || CBlock (SER_DISK, CLIENT_VERSION)`. `CBlockIndex::nDataPos` points at the block bytes, after the 8-byte header.
- The block is the 80-byte header plus `vector<CTransaction>`.
- Dash transactions put `nType` in the high 16 bits of the 32-bit version field (`nVersion` in the low 16 bits). When `nVersion >= 3 && nType != 0`, a trailing `vExtraPayload` (CompactSize-prefixed bytes) follows `nLockTime` (`primitives/transaction.h:346-363`).
- There is no segwit.
- The block hash is **X11** of the 80-byte header.

**rev record** (`UndoWriteToDisk`, `:509-537`):
1. `MessageStart[4] || uint32 LE nSize || CBlockUndo || uint256 checksum`
2. The checksum is `HashWriter` (SHA256d) over `hashBlock || CBlockUndo`, where **`hashBlock` is the hash of the *previous* block** (`pprev->GetBlockHash()`, `:783`; verified in `UndoReadFromDisk` at `:557-569`).
3. `nUndoPos` points after the 8-byte header.
4. Undo data is written in height order (`:786`).

Undo serialization (`src/undo.h`):
- `CBlockUndo = vector<CTxUndo>`, covering every transaction except the coinbase.
- `CTxUndo = vector<Coin>` using `TxInUndoFormatter` (`undo.h:23-52`):
  1. `VARINT(nHeight*2 + fCoinBase)`
  2. if `nHeight > 0`, a legacy byte `0x00` (read back as `VARINT nVersionDummy`)
  3. `TxOutCompression(out)`, described in E.4.

**Import:** required. Raw blocks can be read directly; the X11 hash is stored in the index (E.3).

### E.3 `blocks/index/` (CBlockTreeDB), not obfuscated

Key prefixes (`txdb.cpp:29-50`):

| Key | Value | Notes |
|---|---|---|
| `'b' + uint256 blockhash` | `CDiskBlockIndex` | |
| `'f' + int32 LE nFile` | `CBlockFileInfo`: VARINT × 7 (nBlocks, nSize, nUndoSize, nHeightFirst, nHeightLast, nTimeFirst, nTimeLast) | `chain.h:51-59` |
| `'l'` | int32 last block file | |
| `'R'` | `uint8 '1'` | present while reindexing |
| `'F' + std::string name` | `uint8 '1'/'0'` | names: `prunedblockfiles` (`validation.cpp:3020`, `blockstorage.cpp:393`) and the legacy `addressindex`, `spentindex`, `timestampindex` (`txdb.cpp:508-530`) |
| legacy `'a' 'u' 'p' 's'` | old synchronous address/unspent/spent/timestamp index rows | migrated to `indexes/*` by `CBlockTreeDB::MigrateOldIndexData` (`txdb.cpp:497-…`); copied only if the 'F' flag is true, otherwise discarded |
| legacy `'t'`, `'T'`, `F"txindex"` | old txindex | comment at `txdb.cpp:40-43` |

**`CDiskBlockIndex`** (`chain.h:391-413`):
1. `VARINT(clientVersion, NONNEGATIVE_SIGNED)`, skipped for `SER_GETHASH`
2. `VARINT(nHeight)`
3. `VARINT(nStatus)`
4. `VARINT(nTx)`
5. if `nStatus & (HAVE_DATA|HAVE_UNDO)`: `VARINT(nFile)`
6. if `HAVE_DATA`: `VARINT(nDataPos)`
7. if `HAVE_UNDO`: `VARINT(nUndoPos)`
8. **`uint256 hash` (Dash-specific: the cached X11 hash)**
9. `int32 nVersion`, `uint256 hashPrev`, `uint256 hashMerkleRoot`, `uint32 nTime`, `uint32 nBits`, `uint32 nNonce`

`ConstructBlockHash()` uses the stored hash when it is non-null and only falls back to X11 otherwise (`chain.h:415-428`).

`nStatus` bits (`chain.h:81-135`):

| Bits | Meaning |
|---|---|
| `& 7` | validity level: 1 RESERVED, 2 TREE, 3 TRANSACTIONS, 4 CHAIN, 5 SCRIPTS |
| 8 | HAVE_DATA |
| 16 | HAVE_UNDO |
| 32 | FAILED_VALID |
| 64 | FAILED_CHILD |
| **128** | **BLOCK_CONFLICT_CHAINLOCK** (Dash) |
| 256 | ASSUMED_VALID |

`LoadBlockIndexGuts` re-checks PoW on every entry (`txdb.cpp:297-300`).

**Import:** required (or rebuild it by reindexing the blk files).

### E.4 `chainstate/`, obfuscated

Keys (`txdb.cpp:29-37,62-67`):

| Key | Value |
|---|---|
| `'C' + uint256 txid + VARINT(n)` | `Coin` |
| `'B'` | uint256 best block |
| `'H'` | `vector<uint256>{new_tip, old_tip}`, present only mid-flush (crash-recovery marker; `BatchWrite` at `:118-180`) |
| `\0obfuscate_key` | 8-byte key |

The legacy per-transaction key `'c'` (removed in v0.15) causes `NeedsUpgrade()`, which is an init error (`txdb.cpp:52-58`; tested by `feature_unsupported_utxo_db.py`).

**`Coin`** (`coins.h:61-75`): `VARINT(nHeight*2 + fCoinBase) || AmountCompression(nValue) || ScriptCompression(scriptPubKey)`.

**AmountCompression** (`compressor.cpp:149-196`) writes `VARINT(CompressAmount(n))`:
- `n == 0` encodes as `0`.
- Otherwise strip up to 9 trailing decimal zeros, counting them as `e`.
  - If `e < 9`: `d = n % 10` (1..9), `n /= 10`, and the result is `1 + (n*9 + d - 1)*10 + e`.
  - If `e == 9`: the result is `1 + (n - 1)*10 + 9`.
- Decompression is the inverse.

**ScriptCompression** (`compressor.h:62-95`, `compressor.cpp:55-147`) defines `nSpecialScripts = 6` special encodings. Special cases are written raw, with no VARINT around the type byte:

| Type | Script matched | Payload |
|---|---|---|
| 0x00 | P2PKH `76 a9 14 <20> 88 ac` | 20-byte hash160 |
| 0x01 | P2SH `a9 14 <20> 87` | 20-byte hash |
| 0x02 / 0x03 | P2PK with a compressed key `21 <02/03 + 32> ac` | the 32-byte x coordinate (the type byte is the key prefix) |
| 0x04 / 0x05 | P2PK with an uncompressed key `41 04 <x32 y32> ac`, only if `IsFullyValid()` | 32-byte x; type = `0x04 \| (y & 1)`. Decompression rebuilds the full key from `02/03 + x`, which needs secp256k1 point decompression |
| other | any other script | `VARINT(script.size() + 6)` then raw bytes. On read, if `size - 6 > MAX_SCRIPT_SIZE (10000)` the script becomes a single `OP_RETURN` and the bytes are skipped |

Reading works because the leading VARINT, if it is less than 6, *is* the type byte (`Unser`: `s >> VARINT(nSize)`, with `GetSpecialScriptSize` returning 20 for types 0/1 and 32 for types 2-5).

**Snapshot chainstate:** `chainstate_snapshot/` exists plus a `base_blockhash` file holding a 32-byte uint256 (`node/utxo_snapshot.h:51,66`, `.cpp:75-96`).

**Import:** required. Alternatively the chainstate can be rebuilt by replaying blk/rev files, which is what `-reindex-chainstate` does.

### E.5 `evodb/` (CEvoDB), not obfuscated, CompactSize-string prefixes

CEvoDB layers in-memory `CDBTransaction`s over the raw DB. Values are serialized with `SER_DISK, CLIENT_VERSION`. `WriteDerived` refuses to overwrite a key whose existing value has different bytes (`evodb.h:151-200`).

| Key | Value | Source |
|---|---|---|
| `"b_b4"` | uint256 evo best block. Earlier formats used `b_b`, `b_b2`, `b_b3` | evodb.h:23-27; evodb.cpp:115-135 |
| `("b_b4", uint8 1)` | snapshot-identity best block | evodb.cpp:117,135,199 |
| `"b_dcs"` | uint8 1, dual-chainstate marker | evodb.h:32, .cpp:141 |
| `"b_dcs_mn"` | uint256 snapshot base MN list hash | :33, .cpp:162 |
| `"b_dcs_bg_mn"` | `pair<uint256 block, uint256 mnlist_hash>` | :34, .cpp:176 |
| `("dmn_S3", blockhash)` | `CDeterministicMNList` full snapshot, written every `DISK_SNAPSHOT_PERIOD = 576` blocks | deterministicmns.cpp:31,723; .h:760 |
| `("dmn_D4", blockhash)` | `CDeterministicMNListDiff` (nVersion-first format) | :32,716 |
| `("dmn_D3", blockhash)` | legacy diff, converted by `MigrateLegacyDiffs` at startup (`node/chainstate.cpp:286`) | :33,1044-1160 |
| `"dmn_R1"` | int 1, repaired flag | :34,1298-1304 |
| `("mnhf_s2", blockhash)` | `AbstractEHFManager::Signals` (map bit to height); legacy `mnhf_s` is read and upgraded | mnhftx.cpp:29-30,348-369 |
| `("cpm_S", blockhash)` | `CCreditPool` snapshot | creditpool.cpp:29,131,148 |
| `("q_mc", (uint8 llmqType, uint256 quorumHash))` | `pair<CFinalCommitment, uint256 minedBlockHash>` | llmq/blockprocessor.cpp:46,432 |
| `("q_mcih", uint8 type, BE32(0xffffffff - minedHeight))` | int quorum base height | :47,302,440 |
| `("q_mcihi", uint8 type, int quorumIndex, BE32(inv height))` | int quorum base height (rotation) | :48,308,438 |
| `"q_bbu2"` | uint256, best block of the commitment-index upgrade | :50,219,292,559 |
| `("llmq_S", snapshotHash)` | `CQuorumSnapshot` (rotation), written to the raw DB | llmq/snapshot.cpp:24,312,327 |
| legacy `("q_Qqvvec", key)` and `("q_Qsk", key)` | moved to `llmq/quorumdb` by `MigrateOldQuorumDB` ("remove in v23") | quorumsman.cpp:648-697 |

**Import:** required together with chainstate, because the DIP3 tip check must pass. Alternatively regenerate by `-reindex-chainstate`, which wipes evodb (`init.cpp:2041`, `wipe = fReindex||fReindexChainState`).

### E.6 `llmq/*` LevelDBs, not obfuscated

| DB | Keys and values | Import |
|---|---|---|
| `llmq/quorumdb` | `("q_Qqvvec", uint256 MakeQuorumKey)` → raw vvec DataStream; `("q_Qsk", key)` → `CBLSSecretKey`. `MakeQuorumKey` = SHA256d of `type ‖ quorumHash ‖ each member proTxHash` (quorums.cpp:17-29,194-231) | nice-to-have (can be recovered via QGETDATA or `-llmq-qvvec-sync`) |
| `llmq/isdb` | `"is_v"` int version (CURRENT_VERSION=1; an older version recreates the DB, db.cpp:37-47, db.h:37) · `("is_i", islockHash)` → `InstantSendLock` · `("is_tx", txid)` → islockHash · `("is_in", COutPoint)` → islockHash · `("is_m", BE32(0xffffffff-h), hash)` → true (mined) · `("is_a1", BE32 inv h, hash)` → true and `("is_a2", hash)` → true (archived) (db.cpp:12-26,57-115) | nice-to-have |
| `llmq/recsigdb` | `("rs_r", type, id)` → `CRecoveredSig` · `("rs_r", type, id, msgHash)` → 1 · `("rs_h", recSigHash)` → (type,id) · `("rs_s", signHash)` → 1 · `("rs_m", type, id, msgHash)` → hash · `("rs_t", BE32 time, type, id)` → 1 · `("rs_u", BE32 time, hash)` → 1 · `("rs_p", hash)` → recsig (signing.cpp:34-140; aged out by `-maxrecsigsage`, init.cpp:2082) | regenerate |
| `llmq/dkgdb` | `("qdkg_V", type, quorumBaseHash, proTxHash)` → vvec · `("qdkg_S", …)` → `CBLSSecretKey` contribution · `("qdkg_E", …)` → encrypted contributions (dkgsessionmgr.cpp:24-26,132-142) | regenerate (masternode-only) |

ChainLocks and governance have no LevelDB. The best ChainLock is held in memory and re-learned from peers.

### E.7 `indexes/`, not obfuscated, all optional

Every index stores `'B'` → `CBlockLocator` (`index/base.cpp:26,51-60`).

| Index | Keys and values |
|---|---|
| txindex | `'t' + txid` → `CDiskTxPos` = FlatFilePos{VARINT nFile, VARINT nPos} + `VARINT nTxOffset` (txindex.cpp:14; disktxpos.h:15-18) |
| blockfilter/basic | `db/`: `'t' + BE32 height` → DBVal{uint256 filterHash, uint256 header, FlatFilePos}; `'s' + uint256 hash` → DBVal (reorged-out blocks); `'P'` → next FlatFilePos; **`'V'` → int version, CURRENT_VERSION = 2** (Dash-specific; an older version wipes and rebuilds; blockfilterindex.cpp:33-36,117-127, .h:33). Flat files `fltr%05u.dat` (max 16 MiB, 1 MiB chunks) hold records `uint256 blockHash ‖ vector<uint8> encodedFilter` with **no magic header**; the hash is SHA256d of the encoded filter (:189-260) |
| coinstats | `db/`: `'t'+BE32 h` / `'s'+hash` → DBVal{muhash, txout count, bogo size, 10 amounts…} (coinstatsindex.cpp:25-62); `'M'` → MuHash3072 |
| addressindex | `'a'` + CAddressIndexKey (66 bytes: u8 type{1=P2PK/P2PKH, 2=P2SH} ‖ uint160 ‖ BE32 height ‖ BE32 txpos ‖ txhash ‖ LE32 index ‖ u8 spent) → int64 amount · `'u'` + CAddressUnspentKey (57 bytes: type ‖ uint160 ‖ txhash ‖ LE32 idx) → {int64 amount, CScript, int height} (addressindex_types.h:25-305) |
| spentindex | `'p'` + {txhash, u32 idx} → CSpentIndexValue{txid, idx, height, amount, addrtype, uint160} (spentindex_types.h:38,75-77) |
| timestampindex | `'s'` + {BE32 blockTime ‖ blockhash} (36 bytes) → bool (timestampindex_types.h:57-63) |

**Dash's BIP158 "basic" filter differs from Bitcoin's.** `BasicFilterElements` adds special-transaction payload elements via `ExtractSpecialTxFilterElements` (`blockfilter.cpp:188-215`, `evo/specialtx_filter.cpp`). Filters therefore do not match Bitcoin-spec filters for blocks that contain ProTx or asset-lock transactions. The siphash keys are taken from the X11 block hash.

**Import:** these can all be regenerated. Copying `indexes/*` is optional and would save hours of `-addressindex` rebuild on mainnet.

### E.8 Wallets

**Discovery** (`wallet/db.cpp:25-140`):
- A wallet is a dir containing `wallet.dat`, or a top-level BDB file.
- BDB magic: bytes 12..16 are `0x00053162` (either endianness), file size at least 4096.
- SQLite: header `"SQLite format 3\0"`, `app_id` at offset 68, size at least 512.

**BDB:**
- Berkeley DB **4.8** btree, sub-database name `"main"`, page size 4096. The environment has `database/` log dir and `db.log` (`bdb.cpp:156-171,360,483`).
- `test/functional/test_framework/bdb.py` is a minimal pure-Python reader for 4096-byte btree leaf pages (no overflow pages). It is a useful model for a Rust reader.

**SQLite** (`wallet/sqlite.cpp`):
- One table: `CREATE TABLE main(key BLOB PRIMARY KEY NOT NULL, value BLOB NOT NULL)` (`:320`).
- `PRAGMA application_id = ReadBE32(MessageStart())` as int32, so mainnet is 0xbf0c6bbd interpreted as signed.
- `PRAGMA user_version = WALLET_SCHEMA_VERSION = 0` (`:28,192-206,326-331`).
- Both are checked on open. The network check works because application_id encodes the chain.

**Records:**
- Keys are `serialize(std::string type) ‖ type-specific key`.
- Types include the Bitcoin ones plus Dash-specific ones: `hdchain`, `chdchain`, `hdpubkey`, `g_object`, `cj_salt`, `ps_salt`, `cj_pending_obs`, `platform_data` (`walletdb.cpp:38-75`).
- Wallet `FEATURE_HD = 120200`, `FEATURE_LATEST = FEATURE_HD` (`walletutil.h:18-25`).

**Dump format** (`dash-wallet dump`; `wallet/dump.cpp:20-94`):
1. Line 1: `BITCOIN_CORE_WALLET_DUMP,1`
2. Line 2: `format,<bdb|sqlite>`
3. Then `hexkey,hexvalue` lines
4. Final line: `checksum,<hex SHA256d over all preceding lines>`

**Import:** must-import for wallets. A direct SQLite reader is easy. BDB needs either a 4.8 btree page parser or a conversion path through the dump format.

### E.9 Flat-database files (`CFlatDB<T>`, `src/flat-database.h`)

**Write** (`CoreWrite`, `:38-73`):

```
stream = serialize(std::string strMagicMessage)   // CompactSize + bytes
       ‖ MessageStart[4]
       ‖ T (SER_DISK, CLIENT_VERSION)
file   = stream ‖ uint256 SHA256d(stream)
```

**Read** (`CoreRead`, `:76-156`):
1. Read `file_size - 32` bytes and verify the SHA256d.
2. Check the magic message, then the network magic, then deserialize.

`IncorrectFormat` means the file is recreated. Any other error makes init fail. Each `T` begins with its own `SERIALIZATION_VERSION_STRING`; on a mismatch `Unserialize` returns empty without error.

| File | strMagicMessage | T / version string | Payload after the version string |
|---|---|---|---|
| `governance.dat` | `magicGovernanceCache` | GovernanceStore `"CGovernanceManager-Version-16"` (governance.cpp:31,77) | mapErasedGovernanceObjects, **empty invalid-votes CacheMap (historical)**, cmmapOrphanVotes, mapObjects, mapLastMasternodeObject, CDeterministicMNList lastMNListForVotingKeys (governance.h:201-234) |
| `mncache.dat` | `magicMasternodeCache` | MasternodeMetaStore `"CMasternodeMetaMan-Version-5"` (meta.cpp:11,21) | `vector<CMasternodeMetaInfo>`, int64 nDsqCount, `vector<uint256>` used masternodes (meta.h:100-135) |
| `netfulfilled.dat` | `magicFulfilledCache` | NetFulfilledRequestStore, **no version string** (netfulfilledman.cpp:11) | `map<CService, map<string,int64>>` |
| `sporks.dat` | `magicSporkCache` | SporkStore `"CSporkManager-Version-3"` (spork.cpp:20,42) | mapSporksByHash, mapSporksActive (spork.h:155-175) |

**Import:** all optional. governance.dat is nice-to-have for governance continuity; otherwise it is resynced over P2P (`govsync`).

### E.10 Other node files

| File | Format | Code | Import |
|---|---|---|---|
| `peers.dat` | `MessageStart[4] ‖ AddrMan ‖ SHA256d(all preceding)` (HashedSourceWriter). AddrMan: `u8 format = 4 (V4_MULTIPORT)` ‖ `u8 (32 + lowest_compatible=4)` ‖ uint256 nKey ‖ int nNew ‖ int nTried ‖ int `nUBuckets ^ (1<<30)` ‖ entries (ADDRV2 format). A version error renames the file to `.bak` | addrdb.cpp:37-133,183-227; addrman.cpp:135-234; addrman_impl.h:164-184 | ignore / nice |
| `anchors.dat` | same envelope, `vector<CAddress>` with ADDRV2; deleted after reading | addrdb.cpp:233-252; net.cpp:70 | ignore |
| `banlist.json` | settings-style JSON `{"banned_nets":[{"version":1,"ban_created":…,"banned_until":…,"address":"subnet"}]}`; legacy `banlist.dat` is only noted and ignored | addrdb.cpp:134-180, addrdb.h:36, net_types.cpp:12-66 | nice |
| `mempool.dat` | `u64 version=1` ‖ u64 count ‖ count × (CTransaction, int64 nTime, int64 nFeeDelta) ‖ `map<uint256,CAmount>` deltas ‖ `set<uint256>` unbroadcast. **No XOR key and no v2 format** (Bitcoin v28 obfuscation was not backported). Written as `.new` then renamed | kernel/mempool_persist.cpp:38-130,135-190 | nice (differential tests: mempool_persist, mempool_compatibility) |
| `fee_estimates.dat` | int 140100 (required version) ‖ int CLIENT_VERSION ‖ nBestSeenHeight ‖ heights ‖ buckets (EncodedDouble) ‖ 3 TxConfirmStats. Files older than 60 h are ignored | policy/fees.cpp:405-460,949-980; fees_args.cpp:10 | ignore |
| `settings.json` | JSON object of setting name to value | common/init.cpp:91, util/settings | nice |
| `.cookie` | `__cookie__:<hex>` (`-rpccookiefile`) | rpc/request.cpp:71-76 | ignore |
| `.lock` | flock'd empty file | init.cpp:1477 | ignore |
| `dashd.pid`, `debug.log` | text | init.cpp:202, logging.cpp:16 | ignore |
| `onion_v3_private_key` | Tor `ED25519-V3:<base64>` text | torcontrol.cpp:650 | nice (keeps the .onion address) |
| `i2p_private_key` | binary I2P destination plus private key | net.cpp:4017 | nice |
| `message_capture/<addr>/msgs_{recv,sent}.dat` | repeated records: `u64 LE time_us ‖ char[12] msgtype (NUL-padded) ‖ u32 LE len ‖ payload` | net.cpp:5034-5051; parser at contrib/message-capture/message-capture-parser.py | **a P2P differential-test corpus source** |
| `dash.conf` | config at the datadir root, not per network | — | nice |

Assumeutxo (develop): the `dumptxoutset` file is `SnapshotMetadata{uint256 base, u64 coins_count}` followed by (COutPoint, Coin) pairs (`rpc/blockchain.cpp:3237-3250`, `node/utxo_snapshot.h:43`). An `EvoSnapshot` (version 3: MN list, quorums, historical diffs, modifiers, credit pool, MNHF signals; `evo/snapshot.h:37,501-515`) exists and is referenced by `chainparams.cpp:881,885`, but dumptxoutset does not write it yet. Treat it as in flux.

---

### E.11 Things a Rust reimplementation is likely to get wrong

1. The undo checksum uses the **previous** block's hash, not the block's own hash (`blockstorage.cpp:783,557`).
2. `CDiskBlockIndex` contains a 32-byte block hash between `nUndoPos` and `nVersion`; Bitcoin's layout does not.
3. Only `chainstate` is obfuscated. Still, every LevelDB reader should honour `\0obfuscate_key` if it is present.
4. Evo, LLMQ and IS keys use CompactSize-length **strings** as prefixes. Inverse-height keys are `htobe32(0xffffffff - h)`.
5. The blockfilter index has a `'V'` version (2) and Dash-specific filter elements, so it is not interchangeable with BIP158-spec filters.
6. Old address, spent and timestamp index data may still be in `blocks/index` under `'a'`, `'u'`, `'p'`, `'s'` with `F` flags. An importer has to handle that migration path.
7. Flat DBs use `std::string` magic, then the 4-byte network magic, then a typed version string, then a trailing SHA256d. `netfulfilled.dat` has no version string.
8. mempool.dat is version 1 with no XOR key. peers.dat is format 4 with a lowest-compatible byte of 36.
9. evodb and chainstate must agree on the tip. Import them atomically, or reindex the chainstate (which wipes evodb).

---

## F. Test assets

### F.1 Functional tests (`test/functional/`)

`test/functional/` has 268 `.py` files: 264 tests plus `test_runner`, `create_cache`, `combine_logs` and `example_test`.

| Prefix | feature | interface | mempool | mining | p2p | rpc | wallet | tool |
|---|---|---|---|---|---|---|---|---|
| Count | 79 | 10 | 15 | 3 | 52 | 46 | 58 | 1 |

About 56 tests are Dash-specific. The remaining roughly 208 are backported Bitcoin tests, many adapted (no segwit, X11, DIP3 regtest params). The authoritative list of Dash-only source files is `test/util/data/non-backported.txt`.

**Dash-specific functional tests**, with descriptions taken from their docstrings:

| Test | Description |
|---|---|
| feature_addressindex | `-addressindex` RPCs (getaddressbalance/utxos/deltas/txids/mempool) |
| feature_spentindex | `-spentindex` and getspentinfo |
| feature_timestampindex | `-timestampindex` and getblockhashes |
| feature_asset_locks | DIP-0027 asset lock/unlock special txs, credit pool, withdrawal limits |
| feature_dip3_deterministicmns | DIP3 ProRegTx/ProUpServ/ProUpReg/ProUpRev and the deterministic MN list |
| feature_dip4_coinbasemerkleroots | DIP4 CbTx merkleRootMNList/merkleRootQuorums |
| feature_governance | governance proposals, superblocks, triggers |
| feature_governance_cl | governance checks skipped for blocks covered by the best ChainLock |
| feature_governance_objects | governance object handling |
| feature_governance_txindex_devnet | governance validation refuses to run without txindex outside regtest |
| feature_llmq_chainlocks | LLMQ-based ChainLocks |
| feature_llmq_chainlocks_automatic | recover ChainLocks from blocks without CLSIG messages |
| feature_llmq_connections | intra-quorum connections |
| feature_llmq_data_recovery | automated DKG data recovery, `-llmq-data-recovery`, `-llmq-qvvec-sync` |
| feature_llmq_dkg_intake | adversarial DKG message intake: observers, non-MNAuth peers, oversized payloads |
| feature_llmq_dkgerrors | simulated DKG errors (bad contributions, complaints, justifications) |
| feature_llmq_evo | EvoNodes (HPMN) |
| feature_llmq_is_cl_conflicts | conflicts between ChainLocks and InstantSend |
| feature_llmq_is_retroactive | retroactive IS signing under mempool inconsistencies |
| feature_llmq_rotation | DIP24 quorum rotation |
| feature_llmq_signing | LLMQ signing sessions and recovered sigs |
| feature_llmq_simplepose | PoSe based on LLMQ commitments |
| feature_llmq_singlenode | single-node quorum creation and signing |
| feature_masternode_params | MN mode auto-enables params and refuses duty-skipping settings |
| feature_masternode_params_devnet | MN refuses a stricter relay policy outside regtest |
| feature_masternode_payout_shares | v4 masternode owner payout shares |
| feature_masternode_shares | shared collateral, reward split, dissolution |
| feature_mnehf | MnEhf special tx and EHF signalling (DIP-0023) |
| feature_new_quorum_type_activation | new quorum type activation via a bip9-like fork |
| feature_protx_version | ProTx versioning across v19 (basic BLS) and v24 (extended addresses) |
| feature_quorum_proof_chain | real testnet quorum proofs against an independent checkpoint (uses data/quorum_proof.json) |
| feature_sporks | spork propagation, getsporks, persistence |
| feature_stats | statsd reporting |
| interface_zmq_dash | Dash ZMQ topics (chainlock, islock, gov, recsig…) |
| p2p_connect_to_devnet | connecting to the same devnet by name |
| p2p_dstx | CoinJoin DSTX handling and misbehavior penalties near V24 |
| p2p_govsync_bloom | rejects an oversized govsync bloom filter |
| p2p_instantsend | InstantSend double-spend prevention, ISDLOCK relay |
| p2p_platform_ban | PLATFORMBAN p2p message |
| p2p_quorum_data | QGETDATA/QDATA |
| p2p_sendheaders_compressed | headers2/sendheaders2 (DIP-0025 compressed headers) |
| rpc_coinjoin | coinjoin RPCs |
| rpc_external_queue | docstring says "platform user commands are filtered", but the test actually exercises `-rpcexternaluser` (separate HTTP work queue) for a user named `platform-user`; `-deprecated-platform-user` itself is a no-op |
| rpc_masternode | `masternode` RPC subcommands |
| rpc_mnauth | mnauth RPC (fake MNAUTH) |
| rpc_netinfo | network-info fields across RPCs (extended addresses) |
| rpc_quorum | `quorum` RPC subcommands |
| rpc_verifychainlock | gettxchainlocks and verifychainlock |
| rpc_verifyislock | verifyislock |
| rpc_wipewallettxes | wipewallettxes |
| wallet_dash_rpcs | wallet-dependent Dash RPCs (gobject prepare/submit/vote-many/vote-alias, protx with wallet) |
| wallet_dust_protection | `-dustprotectionthreshold` auto-locking |
| wallet_keypool_hd | HD keypool |
| wallet_mnemonicbits | `-mnemonicbits` |
| wallet_upgradetohd | the upgradetohd RPC |
| wallet_orphanedreward | orphaned block rewards in the wallet |

Bitcoin tests that are heavily Dash-modified: `wallet_hd` (BIP39/BIP44 hdchain), `wallet_backwards_compatibility` (Dash releases 21.1.1, 20.1.1, 19.3.0, 18.2.2, 0.17.0.3, 0.16.1.1), `mempool_compatibility`, `feature_unsupported_utxo_db` (previous releases via `test/get_previous_releases.py`), `p2p_v2_*`, `rpc_dumptxoutset`, and `feature_index_prune`.

Tests that exercise **datadir formats** and so are useful for import compatibility: `feature_reindex`, `feature_loadblock` (`contrib/linearize`), `feature_dbcrash`, `feature_pruning`, `feature_blocksdir`, `feature_txindex`, `feature_coinstatsindex`, `feature_utxo_set_hash`, `mempool_persist`, `feature_addrman`, `feature_anchors`, `rpc_setban`, `feature_settings`, `tool_wallet`, `wallet_backup`, `wallet_migration`, `wallet_descriptor`, `wallet_crosschain` (application_id network check), `p2p_message_capture`, `feature_index_prune`, `rpc_dumptxoutset`, `feature_remove_pruned_files_on_startup`.

### F.2 Test framework Dash support

**`DashTestFramework`** is in `test_framework/test_framework.py:1495+`.
- `add_nodes` appends `dip3params=2:2`, `testactivationheight=v20@N` and `mn_rr@N` (`append_dip3_config`, `:1527-1531`).
- Node 0 gets `sporkkey=cP4EKFyJsHT39LDqgdcB43Y3YXjNyjb5Fuas1GQSeAtjnZWmZEQK` (`:1515`).
- `set_dash_test_params(num_nodes, mn_count, extra_args, evo_count)` defaults: llmq_size 3, threshold 2, dip0024 size 4, v20 and mn_rr at height 100 (`:1532-1563`).
- `set_dash_llmq_test_params` adds `-llmqtestparams`, `-llmqtestinstantsendparams` and `-llmqtestplatformparams` (`:1611-1617`).

Helpers include `activate_by_name/v20/mn_rr`, `create_simple_node`, `dynamically_add_masternode`, `dynamically_prepare_masternode`, `dynamically_evo_update_service`, `prepare_masternodes/prepare_masternode`, `dynamically_start_masternode`, `wait_for_instantlock`, `wait_for_chainlocked_block[_all_nodes]`, `wait_for_sporks_same`, `mine_quorum(llmq_type_name="llmq_test", llmq_type=100, …)`, `mine_quorum_single_member`, `mine_cycle_quorum` (`:1566-2398`), and `MasternodeInfo` (`:1179`).

**Cached chain:** 199 blocks with mocktime `TIME_GENESIS_BLOCK + 199*156` (`:904-1002`).

Other framework modules: `masternodes.py`, `governance.py`, `blocktools.py`, `v2_p2p.py` (BIP324), `bdb.py`, and `crypto/` (chacha20, poly1305, ellswift, muhash, siphash, secp256k1, plus BIP324 CSV vectors: `ellswift_decode_test_vectors.csv`, `xswiftec_inv_test_vectors.csv`, `bip340_test_vectors.csv`).

**`messages.py`** (90 KB) is a Python serializer and deserializer for Dash P2P messages and could be used to generate vectors.
- Primitives and structures: CService, CAddress, CInv, CTransaction (with nType and vExtraPayload), CBlockHeader (hash via `dash_hash.getPoWHash`, the X11 function at `:96-97`), **CompressibleBlockHeader** (headers2), CCbTx, CAssetLockTx, CAssetUnlockTx, CMnEhf, CSimplifiedMNListEntry, CFinalCommitment, CFinalCommitmentPayload, CGovernanceObject, CGovernanceVote, CRecoveredSig, CSigShare, CBLSPublicKey, CBLSIESEncryptedSecretKey, CCoinJoinBroadcastTx, CSigSharesInv.
- Message types: version, verack, addr, addrv2, sendaddrv2, inv, getdata, getblocks, tx, **dstx**, block, getaddr, ping, pong, mempool, notfound, sendheaders, **sendheaders2**, getheaders, **getheaders2**, headers, **headers2**, merkleblock, filterload, filteradd, filterclear, sendcmpct, cmpctblock, getblocktxn, blocktxn, **getmnlistd, getqrinfo, mnlistdiff, clsig, isdlock, platformban, qsigsinv, qsigshare, qsendrecsigs, qwatch, qgetdata, qdata**, the cfilter family, sendtxrcncl.

**Caveat when using messages.py as a vector oracle:**
- `CTransaction.deserialize` reads `vExtraPayload` whenever `nType != 0` (`messages.py:555-576`). C++ also requires `nVersion >= 3` (`primitives/transaction.h:362`).

The **`dash_hash`** package (github.com/dashpay/dash_hash, installation in `test/README.md:31-34`) is required by many Dash tests and by `contrib/linearize`.

### F.3 Unit-test data vectors (`src/test/data/`)

| File | Size | Tests |
|---|---|---|
| script_tests.json | 271,570 | script interpreter vectors, including DIP0020 re-enabled opcodes (CAT, SPLIT, NUM2BIN, BIN2NUM, CHECKDATASIG…; 123 matching lines); no segwit or taproot |
| sighash.json | 205,029 | legacy SignatureHash vectors (raw tx, script, index, hashType, result) |
| tx_valid.json / tx_invalid.json | 45,636 / 38,802 | tx validity with flags (NONE, CLEANSTACK, CONST_SCRIPTCODE, LOW_S, DERSIG, STRICTENC, NULLDUMMY, NULLFAIL, SIGPUSHONLY) |
| trivially_valid.json / trivially_invalid.json | 23,462 / 10,965 | **Dash:** `[txhash, type ("proregtx", …), variant ("basic", …), rawtx hex]` special-tx trivial validation (`evo_trivialvalidation.cpp`) |
| blockfilters.json | 14,635 | BIP158 vectors **recomputed for Dash**: block hashes are X11, e.g. genesis row hash `10eeca34…` |
| key_io_valid.json / key_io_invalid.json | 11,609 / 3,899 | Dash base58 addresses and WIF (X…, 7…, testnet y…, 8…), generated by `contrib/testgen/gen_key_io_test_vectors.py` |
| base58_encode_decode.json | 1,462 | base58 |
| bip39_vectors.json | 11,041 | **Dash:** BIP39 entropy, mnemonic, seed, xprv (TREZOR passphrase) |
| proposals_valid.json / proposals_invalid.json | 1,501 / 4,250 | **Dash:** governance proposal JSON validator |
| asmap.raw | 59 | asmap |

**Dash-specific unit tests** (`src/test/`; number of cases in parentheses; vector-bearing ones marked ★):
- ★`pow_tests` (14): DGW `get_next_work` using 24 real mainnet (time, nBits) pairs before block 123457, expecting `0x1b1441de`, plus devnet slow-block rules (`0x1c00c8f8`, `0x207fffff`) (`pow_tests.cpp:16-66`)
- ★`subsidy_tests` (1): `GetBlockSubsidyInner(prevBits, prevHeight, v20)` against real mainnet heights 4249/4501/5464/5465/17588/99999/210239+ (`subsidy_tests.cpp:14-100`)
- ★`dip14_tests` (7): DIP14 256-bit derivation vectors 1-4 and BIP32 compatibility
- ★`evo_simplifiedmns_tests` (2): SML merkle-root vector
- ★`evo_islock_tests` (13): real ISLOCK deserialization, request-ID format
- ★`bls_tests` (20)
- ★`quorum_proofs_tests` (11): 70 hex literals including real testnet wire data
- ★`evo_snapshot_tests` (31)
- `block_reward_reallocation_tests` (2)
- `dip0020opcodes_tests` (5: bitwise, string, type-conversion, div/mod)
- `checkdatasig_tests`
- `dynamic_activation_thresholds_tests`
- `evo_assetlocks_tests` (12), `evo_cbtx_tests`, `evo_db_tests` (9), `evo_deterministicmns_tests` (40, 3664 lines), `evo_mnauth_tests`, `evo_mnhf_tests`, `evo_netinfo_tests` (16), `evo_sharedmn_tests` (13), `evo_trivialvalidation`, `evo_utils_tests`
- `governance_*` (inv, superblock, validators, vote_processing, vote_sync, vote_wire)
- `llmq_*` (blockprocessor, chainlock 13, commitment 11, dkg, hash 6, invalid_type, params 8, qgetdata, snapshot 13, utils 17)
- `masternode_params_tests`, `masternode_payments_tests` (7), `spork_tests` (3)
- `coinjoin_{dstxmanager,inouts(52),queue(11)}_tests`, `ratecheck_tests`
- `cachemap/cachemultimap/unordered_lru_cache/limitedmap_tests`

There are no standalone X11 vectors in the unit tests. For X11, use the genesis hashes in `chainparams.cpp`, the blockfilters.json hashes, and `dash_hash`.

Format-relevant Bitcoin tests: `compress_tests`, `coins_tests` (CCoinsViewDB plus hex Coin vectors), `dbwrapper_tests` (obfuscation), `serialize_tests` (VARINT and CompactSize), `flatfile_tests`, `blockfilter_tests`, `addrman_tests` (peers.dat serialization), `streams_tests` (Xor).

### F.4 Fuzz targets (`src/test/fuzz/`, 115 files)

**Deserialization targets** in `deserialize.cpp`, which suit differential fuzzing (round-trip against the Rust codec):
block_filter, addr_info, block_file_info, block_header_and_short_txids, fee_rate, merkle_block, out_point, partial_merkle_tree, pub_key, script, tx_in, flat_file_pos, key_origin_info, psbt (+input/output), prefilled_transaction, block, blocklocator, blockmerkleroot, **addrman**, blockheader, **txundo, blockundo, coins, diskblockindex, txoutcompressor**, netaddr, service, messageheader, address v1 (no time / with time) and v2, inv, bloomfilter, blocktransactions[request], **snapshotmetadata**, uint160, uint256.

Other relevant targets: `decode_tx`, `transaction`, `primitives_transaction`, `tx_in`, `tx_out`, `script*`, `eval_script`, `script_interpreter`, `block`, `block_header`, `blockfilter`, `golomb_rice`, `coins_view`, `coinscache_sim`, `utxo_snapshot`, `load_external_block_file`, `policy_estimator_io`, `p2p_transport_serialization`, `bip324`, `process_message` / `process_messages`, `validation_load_mempool`, `autofile`, `buffered_file`.

The only Dash-specific fuzz target is **`dkg_message_framing`** (QCONTRIB, QCOMPLAINT, QJUSTIFICATION, QPCOMMITMENT). There are no fuzz targets for ProTx, CbTx, ISLOCK, CLSIG or governance deserialization; those would have to be written. `test/fuzz/test_runner.py` exists, but no corpora are checked in.

### F.5 Regtest and devnet tooling

From `chainparams.cpp:1385-1408` and `init.cpp:826-833`:

| Option | Scope | Meaning |
|---|---|---|
| `-devnet=<name>` / `-chain=devnet` | | named devnet; datadir `devnet-<name>` |
| `-budgetparams=<mn>:<budget>:<superblock>` | regtest | override start heights |
| `-dip3params=<activation>:<enforcement>` | regtest | DIP3 heights (tests use 2:2) |
| `-testactivationheight=name@h` | regtest | bip147, bip34, dersig, cltv, csv, brr, dip0001, dip0008, dip0024, v19, v20, mn_rr |
| `-vbparams=dep:start:end[:min_act[:window:threshold[:thresholdmin:falloff:mnactivation]]]` | regtest | BIP9 / EHF parameters |
| `-llmqtestparams`, `-llmqtestinstantsendparams`, `-llmqtestplatformparams=<size>:<thr>` | regtest | default 3:2 |
| `-llmqtestinstantsenddip0024=<name>` | regtest | |
| `-highsubsidyblocks`, `-highsubsidyfactor`, `-minimumdifficultyblocks`, `-powtargetspacing`, `-llmqchainlocks`, `-llmqinstantsenddip0024`, `-llmqplatform`, `-llmqmnhf`, `-llmqdevnetparams=<size>:<thr>` | devnet | |
| `-sporkkey`, `-sporkaddr` | | `-minsporkkeys` no longer exists in develop |
| `-llmq-data-recovery`, `-llmq-qvvec-sync`, `-maxrecsigsage` | | LLMQ data handling |
| `-fastprune`, `-tinyblk`, `-capturemessages`, `-dbcrashratio`, `-dbbatchsize`, `-forcecompactdb` | | testing |

`contrib/` contents:
- `linearize/` (linearize-hashes and linearize-data; needs dash_hash; produces bootstrap blk files for `-loadblock`)
- `message-capture/message-capture-parser.py`
- `testgen/gen_key_io_test_vectors.py`
- `devtools/` (utxo_snapshot.sh, gen-dash-conf.sh, test_deterministic_coverage.sh…)
- `seeds/` (nodes_main.txt, nodes_test.txt, onion_seeds.txt)
- `zmq/`, `tracing/` (USDT; used by interface_usdt_*)

There is no checked-in regtest datadir snapshot. The rust-dashcore `contrib/setup-dashd.py` downloads one separately.

### F.6 Shared corpora

**`test/functional/data/`:**

| File | Size | Used by |
|---|---|---|
| rpc_getblockstats.json | 42,652 | rpc_getblockstats: a regtest chain plus expected stats, a good RPC differential fixture |
| rpc_psbt.json | 49,534 | |
| blockheader_testnet3.hex | 41,387 | p2p_dos_header_tree: real Dash testnet headers |
| quorum_proof.json | 7,610 | feature_quorum_proof_chain: real testnet checkpoint and target heights 1548500 → 1549547 |
| rpc_bip67.json | 3,427 | |
| rpc_decodescript.json | 558 | |
| invalid_txs.py | — | p2p_invalid_tx / feature_block |

**Other corpora:**
- `test/util/data/` has 40 files: `bitcoin-util-test.json` with tx and txcreate hex/json pairs for dash-tx CLI differential testing, and `non-backported.txt`.
- `src/bench/data/block813851.raw` (749,213 bytes) is a raw serialized block used by the CheckBlock bench. The filename suggests mainnet height 813851, but the file does not say so; confirm by X11-hashing it.
- `test/lint/` has standard lints plus `lint-cppcheck-dash.py`.

---

---

## G. Cross-cutting pitfalls, inconsistencies found, open questions

### G.1 Cross-cutting implementation pitfalls

These are consolidated from A.1–A.6. Each one is something a Rust port is likely to get wrong.

1. **Model the BLS scheme as explicit context, never as a global.** Dash Core flips `bls::bls_legacy_scheme` per block, at V19. That flag decides the default serialization of `CbTx.bestCLSignature`, recovered sigs, MNAUTH, and `BuildCommitmentHash`'s pubkey. It does *not* decide ProTx, DMN-state or commitment-field encodings, which use explicit version wrappers. Bytes in the opposite scheme deserialize to an *invalid* object rather than an error.
2. **"Active at" vs "active after".** Most gates use `DeploymentActiveAfter(pindexPrev)`. Several use `DeploymentActiveAt(*pindex)` or `DeploymentActiveAt(*pindexPrev)`: CbTx DIP8, the credit pool, the MNHF cache, the shared-collateral spend check, and the V20 test in quorum modifier/list selection. The V20 checks for the list and for the modifier are made at *different* blocks. Copy each gate exactly.
3. **Ordering is consensus.** Within `ConnectBlock`, special-tx processing (DML, quorums, CbTx roots) runs *before* UTXO updates. The DML mutation order inside a block is: confirm, decay PoSe, special txs in block order, collateral spends, payee bump, consecutive-payment reset. Map-iteration and tie-break orders are observable; uint256 ordering is a memcmp of the little-endian bytes. That covers SML sort, payee tie-break, quorum selection, `GetBestSuperblock` and approved-proposal ordering.
4. **Floating point in consensus.** Subsidy (`pow()`, truncation, mainnet ≤4500 bug), KGW, and the mainnet ≤68589 difficulty tolerance all use floating point. Either replicate glibc `pow` bit-exactly for the historical range, or embed known-good outputs and checkpoint past them.
5. **Hashes that are not "hash of the serialization".**
   - Governance object hash: data as a hex string, plus a vin-like suffix, including the sig and excluding the collateral hash.
   - Governance vote hash: signal before outcome.
   - Stable txid of asset-unlock v2: the trailing 132 bytes are zeroed.
   - `confirmedHashWithProRegTxHash` and the quorum score: single SHA256.
   - `SignHash`: double SHA256 over `type‖quorumHash‖id‖msgHash`.
   - Request IDs are CompactSize-prefixed strings: `clsig`, `inlock`, `islock`, `mnhf`, `plwdtx`, `PlatformPoSeBan`.
6. **Count encodings differ.** Payloads use u8 counts (payouts, shares, sigs). DMN state uses CompactSize. Diffs use VARINT. `DYNBITSET` rejects padding bits. `AUTOBITSET` picks between fixed and gap-encoded per message.
7. **State-dependent validity.** These outcomes depend on governance sync state, ChainLock state, IS state and IBD status:
   - `bad-cb-amount` / `bad-cb-payee` failures are not punished, but are cached as `FAILED_VALID`.
   - Conflicting-IS rejection applies only outside IBD.
   - Superblock checks are skipped when the block is ChainLocked or the node is not synced.
   - If no payee can be computed, any coinbase is accepted.

   A Rust node must run the same state machines and must treat ChainLocks as final in order to converge.
8. **Side-effect state.** DIP24 snapshots are created while connecting cycle-base blocks, and are *not* reverted on reorg because they are keyed by block hash. Whether one exists depends on the enabled-MN count. ChainLocks are not persisted; they are rebuilt from coinbases and peers.
9. **assumevalid scope.** Below assumevalid, Dash also skips CbTx merkle roots, best-CL checks, ProTx signature checks and BLS commitment-signature checks. The DMN list is still built.
10. **Network fingerprint.** See A.2 "Notes for a Rust reimplementation". The most visible differences:
    - the `version` extra fields
    - the service-bit defaults: 0x1C05 for a node, 0x1C45 for a masternode
    - the post-verack message sequence (`sendheaders2` instead of `sendheaders`, `senddsq`)
    - automatic `getsporks`, `mempool` and `govsync` traffic
    - per-peer-version BIP324 short-ID use
    - 100-point misbehavior scoring
    - disconnect after `full-sync`
    - masternode-specific timing (1 s inbound protection for MNAUTH, a 100 ms send cadence)

### G.2 Inconsistencies and surprises found while surveying

- **Docstring and option mismatch.** `test/functional/rpc_external_queue.py` says it tests filtering of platform-user commands. In fact it tests `-rpcexternaluser` work-queue routing. `-deprecated-platform-user` is registered (`init.cpp:835`) but never read.
- **Swapped help text.** The `-help` strings for `-zmqpubrawgovernancevote` and `-zmqpubrawgovernanceobject` are swapped (`init.cpp:733-734`).
- **Stale RPC help.** `gobject prepare` help still mentions a 5 DASH fee; the fee is 1 DASH on every network.
- **Removed option.** `-minsporkkeys` is gone; only one spork address is used. A Qt translation string still references it.
- **Retired opcode flag.** `SCRIPT_ENABLE_DIP0020_OPCODES` is retired; the DIP0020 opcodes are valid from genesis. The `dip0020` deployment height now only gates enabling LLMQ_100_67.
- **Retired LLMQ role.** There is no longer a non-rotating `llmqTypeInstantSend` and no `llmqTypeAssetLocks`. Asset unlocks use the Platform quorum type.
- **Removed persistence.** Address, spent and timestamp indexes moved from `blocks/index` to async `indexes/*` databases; an old-layout migration runs at startup. The BIP158 filter index has a `'V'` version key (version 2) because Dash adds special-tx elements.
- **Unbackported Bitcoin features.** These Bitcoin features are absent: `xor.dat`/`-blocksxor`, mempool.dat v2 XOR, headers presync, `feefilter`, wtxid relay, RBF, `bumpfee` and JSON-RPC 2.0. Erlay code is present but `-txreconciliation` defaults to off.
- **Default that differs from Bitcoin.** `-txindex` defaults to **1**. Governance validation refuses to start without it outside regtest unless `-disablegovernance` is set. Masternodes require it.
- **Wallet mnemonic quirks.** The BIP39 checksum check uses XOR (`2 ^ n`) instead of a power. Mnemonic and passphrase are not NFKD-normalised. Wallets created with these quirks exist in the wild, so an importer must accept them.
- **Python codec caveat.** In `messages.py`, the special-tx payload is read whenever `nType != 0`, without the `nVersion >= 3` condition C++ applies, and nVersion is treated as unsigned. Keep this in mind when using it as an oracle.

### G.3 Open questions for the program (decisions, not research gaps)

1. **Which Dash Core baseline to target.** Options: the v24.0.0 release (V24 rules defined, mainnet activation TBD) or track develop (24.1-pre)? The recommendation is to pin a commit per milestone and re-run this inventory's extraction scripts as a diff.
2. **evodb byte compatibility.** Full-list snapshots serialize in immer HAMT order, and `WriteDerived` byte-compares. Options: import-only (read Core's evodb, write our own format), or bit-compatible writes. Recommendation: import-only, plus a "rebuild from blocks" path.
3. **Legacy BLS.** The legacy hash-to-G2 has to be ported from `dashbls/src/legacy.cpp`, or we bind to dashbls via FFI. Historical blocks before V19 need legacy verification, unless we checkpoint or assumevalid past them; DKG and commitment history does not depend on runtime legacy signing after V19.
4. **Governance-dependent block validity.** Full superblock validation needs governance sync. Decide whether the Rust node ships governance from day one, or initially mirrors Core's "skip when not synced or when ChainLocked" path.
5. **Wallet import path for BDB.** Options: a native BDB 4.8 btree page reader (a model exists in `test/functional/test_framework/bdb.py`), or require users to run `dash-wallet dump` first. The SQLite path is straightforward.
6. **Functional-test reuse.** To run Dash's `test/functional` suite against the Rust node, it must accept the regtest and devnet flags (§C.7, A.6 §6), the RPC surface used by `DashTestFramework`, and `-sporkkey`. Decide early, because this is the cheapest end-to-end differential harness available.
