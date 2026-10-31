# R5: Wallet user stories → rust-dashcore capability map

*Research report for the rust-dashcore full-node program. Read-only survey, 2026-10-08.*

## 0. Scope, sources, legend

**Question.** Which end-user stories do Dash users perform today in the mobile wallets, Dash Evo Tool (DET), dash-qt and dash-cli? Which rust-dashcore APIs serve each story, and which are partial or missing? What has to exist so that a Rust node + CLI + a new desktop app (inspired by the mobile wallets) reach user-story parity with Dash Core, and so that DET and the desktop app can drop dashd?

**Sources and revisions surveyed**

| Source | Location / rev | Notes |
|---|---|---|
| rust-dashcore | `~/workspace/rust-dashcore`, `dev` @ `67b307c6` | key-wallet, key-wallet-manager, key-wallet-ffi, dash-spv, dash-spv-ffi, dash, rpc-client/json |
| Android wallet | `~/workspace/dash-wallet` @ `ef552d410` (v11.9.1) | Kotlin. Uses **dashj 22.0.5**, not rust-dashcore. Platform via `dash-sdk-{java,kotlin,android}` 4.0.1 |
| iOS wallet | `github.com/dashpay/dashwallet-ios` `develop` @ `3df12fc8` (inspected via `gh`) | **DashSync has been removed.** It now runs on Platform's `swift-sdk` → Rust FFI |
| Dash Platform | `~/workspace/platform` @ `7b61d6f3de` | `rs-platform-wallet`, `rs-platform-wallet-ffi`, `swift-sdk`, `rs-unified-sdk-ffi` |
| Dash Evo Tool | `~/workspace/dash-evo-tool`, `v1.0-dev` @ `e4850fca` | egui desktop app. Its user stories are in `docs/user-stories.md` (WAL/SND/ALK/IDN/DPN/DPY/MN/NET …) |
| Dash Core | `~/workspace/dash` @ `15db478750` (24.1.0-dev; latest tag v24.0.0-rc.2) | dashd / dash-qt / dash-cli reference. Items marked **[v24]** are new in v24 |

**Abbreviations.** Apps: **AND** = Android dash-wallet, **iOS** = dashwallet-ios, **DET** = Dash Evo Tool, **QT** = dash-qt, **CLI** = dashd RPC / dash-cli.
Status of the rust-dashcore capability: **P** present, **Pa** partial, **M** missing. **(plat)** means the capability exists, but in the Platform repo (`rs-platform-wallet`) rather than rust-dashcore. **(node)** means it needs a full-node subsystem, not just a wallet library.

Paths are relative to the rust-dashcore root unless prefixed.

---

## 1. Ecosystem consumption map (who uses rust-dashcore, and how)

```
dashwallet-ios ─► SwiftDashSDK (platform/packages/swift-sdk)
                    └► DashSDKFFI.xcframework  (rs-unified-sdk-ffi)
                          ├ key-wallet-ffi          (rust-dashcore)
                          ├ platform-wallet-ffi ─► rs-platform-wallet ─► key-wallet, key-wallet-manager, dash-spv
                          ├ rs-sdk-ffi              (DAPI, state transitions, contested-name votes)
                          └ dash-network
Android (new SDK path) ─► rs-unified-sdk-jni (same three FFIs)   [shipping app v11.9.1 still uses dashj]
DET ─► dash-sdk + platform-wallet (git platform@9f7ed169) ─► rust-dashcore (transitively only)
```

- **Nobody uses `dash-spv-ffi`.** iOS and DET reach dash-spv through `rs-platform-wallet::SpvRuntime` (`platform/packages/rs-platform-wallet/src/spv/runtime.rs:109`). That runtime is exported to C as `platform_wallet_manager_spv_*` (`rs-platform-wallet-ffi/src/spv.rs:140-655`).
- **Pinned revisions.** Both consumers are 88 commits behind `dev`.
  - DET's `Cargo.lock` resolves rust-dashcore to `e4208c90` (2026-09-10).
  - Platform `Cargo.toml:68-75` pins `40268cc0`. That commit is **not on `dev`**: it is the tip of `upstream/integration/v4.2-core-v24`, which is `e4208c90` plus 5 rpc-json commits modelling Core v24 shared and multi-payout masternodes.
  - Neither consumer has the breaking `hex-conservative` 1.x refactor (#1115).
- **DET has no direct rust-dashcore dependency.** Everything arrives through `dash_sdk::dpp::key_wallet`, `dash_sdk::dash_spv` and `dash_sdk::dashcore_rpc`. This shapes the node program's API plan: the desktop app's wallet surface today is `rs-platform-wallet`, not key-wallet directly.
- **iOS dropped CoinJoin** in the DashSync → Rust migration. It only reads and sweeps the CoinJoin account balance (`SwiftDashSDKCoinJoinBalanceReader.swift`, "CoinJoin is no longer supported"). It also stopped verifying IS/CL quorum signatures locally (it trusts the SDK). As a result, the only mobile CoinJoin implementation left is dashj (Android).

---

## 2. User-story catalog with capability mapping

Each story has an ID, the apps that support it today and how, the rust-dashcore capability it needs, and that capability's status.

### 2.1 Everyday wallet user (US-W)

| ID | Story | Apps today: how | rust-dashcore capability | Status |
|---|---|---|---|---|
| US-W-01 | Create a new HD wallet, choosing 12 or 24 words | AND (`WalletFactory.kt:106-121`, adds `m/0'`, BIP44, CoinJoin, auth chains); iOS; DET (`bip39::Mnemonic::from_entropy_in`); QT (create + 3-word verification dialog); CLI `createwallet` (v24: descriptors by default) | `key-wallet/src/mnemonic.rs:162-169` (12/15/18/21/24 words, 10 languages). `Wallet::from_mnemonic` (`wallet/initialization.rs:103`). `Wallet::new_random` is hard-wired to 12 English words (`:107`) | **P** (`new_random` is inflexible) |
| US-W-02 | Restore from a recovery phrase, with an optional "created on" date to shorten the rescan | AND (the date drives the checkpoint); iOS; DET (imported wallet birth = 0); QT/CLI (`-mnemonic`, `upgradetohd`, `createwallet` + mnemonic) | `Mnemonic::from_phrase` auto-detects the language. Per-wallet birth/synced height drives the filter rescan (`dash-spv/src/sync/filters/manager.rs:158,771`). The birth-height policy lives in (plat) `wallet_lifecycle.rs:80-95` | **P** / policy in (plat) |
| US-W-03 | Use a BIP39 passphrase ("25th word") | QT/CLI only (`-mnemonicpassphrase`, `upgradetohd`; stored in `hdchain` and descriptor-key records). AND, iOS and DET always use `""` | `Mnemonic::to_seed(pass)` exists (`mnemonic.rs:267`). But `Wallet::from_mnemonic` and `WalletType::Mnemonic` hard-code `""` (`initialization.rs:103-110`, `wallet/mod.rs:36`), and the FFI `wallet_create_from_mnemonic*` has no passphrase. Going through `from_seed` loses the mnemonic | **Pa**. **Blocks Core-wallet import parity** |
| US-W-04 | View or verify the recovery phrase; backup reminders | AND (`ui/verify/*`); iOS; DET; QT ("Show Recovery Phrase"); CLI `dumphdinfo` / `listdescriptors true` | Mnemonic retained in `WalletType::Mnemonic` | **P** (the UI belongs to the app) |
| US-W-05 | Encrypt the wallet; lock/unlock; change passphrase; PIN / biometric | AND (scrypt KeyCrypter + Android Keystore); iOS (keychain); DET (own Argon2id + XChaCha20 vault, `wallet_backend/secret_seam.rs`); QT/CLI `encryptwallet`, `walletpassphrase[change]`, `walletlock` | No at-rest crypto anywhere. `Wallet::backup()` writes plaintext bincode that includes the seed (`key-wallet/src/wallet/backup.rs:29-55`). Every consumer reimplements this | **M** |
| US-W-06 | Receive: fresh address, QR / BIP21 URI with amount and label, list of requests | All. QT stores requests as `destdata rr##`. AND `WalletAddressFragment.java:230` | Address pools with gap limit, labels and reservation (`key-wallet/src/managed_account/address_pool.rs`, `gap_limit.rs:14-55`). `Address::to_qr_uri` only (`dash/src/address.rs:1182`); no BIP21 parser or builder with amount and label | **P** / BIP21 **Pa** |
| US-W-07 | Send to one or more addresses, send max, OP_RETURN | All. DET builds by hand with `TransactionBuilder` because the upstream `send_to_addresses` was removed (`dash-evo-tool/src/wallet_backend/payments.rs:689-790`) | `TransactionBuilder` (`key-wallet/src/wallet/managed_wallet_info/transaction_builder.rs:78`; OP_RETURN `:264`; `add_inputs` `:240`); `build_and_sign_transaction` (`transaction_building.rs:123-158`); `WalletManager::build_and_sign_transaction` (`key-wallet-manager/src/lib.rs:772`). Coin selection supports BnB / LargestFirst / All (`coin_selection.rs:86-115`) | **P**. Subtract-fee-from-amount is **Pa** (only through `All`) |
| US-W-08 | Choose or see the fee; network fee estimation | AND fixed 1000 duff/kB (`common/.../Constants.kt:46`); DET fixed `FeeRate::new(1000)` with its own estimator (`model/fee_estimation.rs`); QT custom fee + `estimatesmartfee` | Static presets in `fee.rs:12-115`. **`economy()` = 500 duff/kB is below Dash's 1000 min relay** (`fee.rs:74-76`). No estimator | **Pa** (node: fee estimation needs mempool and block stats) |
| US-W-09 | See InstantSend / ChainLock status; spend IS-locked coins immediately | All (iOS now trusts SDK status) | IS-lock and ChainLock BLS verification (`dash-spv/src/validation/instantlock.rs:73-119`, `chainlock.rs:46-85`); `TransactionContext::{Mempool, InstantSend, InBlock, InChainLockedBlock}` (`key-wallet/src/transaction_checking/transaction_context.rs:66`); `require_final_inputs` (`transaction_builder.rs:131`) | **P** |
| US-W-10 | Transaction history with confirmations, type, memo and tax category; CSV export | AND (memos, tax categories, Platform-backed metadata, TaxBit/ZenLedger CSV); iOS; DET (event-sourced snapshot); QT (export CSV); CLI `listtransactions` / `listsinceblock` | `TransactionRecord` and accessors (`key-wallet-manager/src/accessors.rs`); classification (`key-wallet/src/transaction_checking/transaction_router/mod.rs:15-43`); events (`key-wallet-manager/src/events.rs:182-383`). Memos and categories are app data | **P** (metadata belongs to the app) |
| US-W-11 | Address book and labels | AND (`AddressBookProvider`); QT; CLI `setlabel` | Labels on the wallet's own pool addresses only (`address_pool.rs`). No contact address book | **Pa** |
| US-W-12 | Coin control: list UTXOs, pick inputs, lock/unlock UTXOs | QT (coin control dialog); CLI `listunspent`, `lockunspent`. Not in AND or DET | UTXO accessors **P**. `TransactionBuilder::add_inputs` **P**. `Utxo::lock/unlock` exists (`key-wallet/src/utxo.rs:104-110`) but has no wallet or FFI API, so `WalletCoreBalance.locked` is always 0. **Collateral auto-lock exists only on the unmerged `upstream/backport/key-wallet-collateral-lock-on-719de34b`** (`beb262b0`, `e49cb973`, `0c0dca37`) | **Pa** |
| US-W-13 | Sign / verify a message | QT; CLI `signmessage` / `verifymessage`; AND (CrowdNode signup); iOS and DET (sign message) | Primitives in `dash/src/sign_message.rs:104-219` (compact sig, recover, `is_signed_by_address`). No key-wallet or FFI API; (plat) has `rs-platform-wallet/src/wallet/core/sign_message.rs` | **Pa** |
| US-W-14 | Sweep or import a paper-wallet key (WIF, BIP38) | AND sweep (UTXOs from **Electrum / Insight servers**, `RequestWalletBalanceTask.java:134-210`); QT/CLI `importprivkey` + rescan; DET imports single keys but **cannot show their balance or spend them** (`SingleKeyWalletsUnsupported`) | `PrivateKey::from_wif` **P**. Imported-key account: **M**. Arbitrary-address UTXO lookup: **M** (node: `scantxoutset`-style UTXO-set scan, or an address index). BIP38 was **removed** in #1111 (`4fc2b4ca`) | **M** |
| US-W-15 | Rescan or resync, optionally from a date or height | AND rescan dialog; DET "clear SPV data"; QT/CLI `rescanblockchain`, `-rescan` | Automatic per-wallet rescan from birth height; (plat) `spv_rescan_filters`, `clear_storage`. No "rescan from height H" API | **Pa** |
| US-W-16 | Several wallets (create, load, unload, switch, rename, remove) | DET (WAL-004..007); QT/CLI `createwallet` / `loadwallet` / `unloadwallet` | `WalletManager` keyed by `WalletId` (`key-wallet-manager/src/lib.rs:134`) | **P** |
| US-W-17 | Several accounts per wallet | Not in AND, Core or DET (account 0 only) | `AccountType::Standard{index}`, `create_account` (`key-wallet-manager/src/lib.rs:680`) | **P** (exceeds the apps) |
| US-W-18 | Watch-only wallet from an xpub, descriptors or addresses | QT/CLI (`disable_private_keys`, `importaddress`, `importdescriptors`). Missing in DET, which blocks WAL-031 / SND-002 | `Wallet::from_xpub` needs per-account xpubs (`initialization.rs:261`). `WalletManager::import_wallet_from_xpub` creates an **empty** account set (`key-wallet-manager/src/lib.rs:482-500`). No address-only watch, no seedless registration in (plat) (`register_wallet` is private) | **Pa** |
| US-W-19 | Show or share an account xpub | AND (Tools → xpub with `?c=…&h=bip44`); QT `listdescriptors` | `wallet_get_account_xpub` (FFI), `derive_extended_public_key` | **P** |
| US-W-20 | Pay a BIP70/72 request, `dash:` / `pay:` URIs, Base43 | AND (`InputParser.java:80-217`), iOS | None in rust-dashcore | **M** (app-level; low priority for the node) |
| US-W-21 | Network status: peers, sync progress, trusted-peer-only mode, choose network | All. AND has code for a trusted peer but no UI | `sync_progress`, peer events, explicit peers + restrict mode (`dash-spv/src/client/config.rs`, `network/`) | **P** |
| US-W-22 | Back up or restore a wallet file | QT/CLI `backupwallet` / `restorewallet` + automatic backups (`-createwalletbackups`); AND encrypted backup in debug builds only; DET vault | `Wallet::backup()` is plaintext bincode | **Pa** (no encrypted format) |
| US-W-23 | PSBT, hardware wallet, external signer | QT (Load PSBT); CLI PSBT RPCs and `register_prepare` | `Signer` trait + `ExtendedPubKeySigner` (`key-wallet/src/signer.rs`); the builder rejects non-digest signers (`transaction_builder.rs:1082`). No PSBT module | **Pa** / PSBT **M** |
| US-W-24 | Abandon a stuck tx; see conflicts | QT/CLI `abandontransaction`; iOS (abandon) | `WalletManager::abandon_transaction` (`key-wallet-manager/src/lib.rs:723`); `TransactionsSwept` event | **P** |
| US-W-25 | Legacy-chain upgrade: an old wallet keeps scanning `m/0'` and gains BIP44 | AND (`MainActivity.kt:526-563`) | `StandardAccountType::BIP32Account` = `m/acct'` (`key-wallet/src/account/account_type.rs:398-402`) | **P** |
| US-W-26 | Fiat rates, buy/sell, gift cards, Explore merchants/ATMs, CrowdNode, Uphold/Coinbase/Topper/Maya | AND (and iOS) via external services | Out of scope for the node (CrowdNode only needs message signing + send) | n/a |

### 2.2 CoinJoin user (US-CJ)

| ID | Story | Apps today: how | rust-dashcore capability | Status |
|---|---|---|---|---|
| US-CJ-01 | Turn mixing on/off and choose a level, rounds or amount | AND: modes NONE / INTERMEDIATE (4 rounds) / ADVANCED (8 rounds), 6 sessions, mix whole balance, denoms goal 50 / hardcap 300 (`CoinJoinService.kt:87-145,550-580`). QT: options page + overview button. CLI: `coinjoin start/stop/reset`, `setcoinjoinrounds`, `setcoinjoinamount`, `coinjoinsalt`. **iOS and DET: none** | No mixing client: no denomination creation, collateral tx, session state machine, queue selection or signing. P2P `dsa/dsi/dsf/dss/dsc/dssu/dstx/dsq` are absent; only `SendDsq(bool)` exists (`dash/src/network/message.rs:275`) | **M** (largest wallet-side gap) |
| US-CJ-02 | See mixing progress, rounds and mixed balance | AND (`MixingStatusCard`, `COINJOIN_SPENDABLE`); QT overview; CLI `getcoinjoininfo`, `coinjoin status` | CoinJoin account balance **P**. `CoinJoinPools.rounds_completed` is never updated (`key-wallet/src/account/coinjoin.rs:20`). No Core-style rounds computation by ancestry walk (Core `wallet/coinjoin.cpp:261-426`, including the `cj_salt` coin-flip) | **Pa** |
| US-CJ-03 | Spend only fully-mixed funds | AND: `CoinJoinCoinSelector`, greedy no-change when the fee is ≤ 0.002 DASH (`SendCoinsTaskRunner.kt:97,117-141`). QT: separate CoinJoin send page | The CoinJoin account is excluded from default sends (`transaction_building.rs:36-44`), and a builder can be funded from it explicitly (`add_funding`). No "≥ N rounds only" filter, no no-change greedy mode | **Pa** |
| US-CJ-04 | See mixing transactions grouped (create denominations, mixing, fee, collateral, combine dust) | AND (`CoinJoinTxResourceMapper.kt:44-49`); QT | Router labels CoinJoin by a heuristic (`transaction_router/mod.rs:203-233`). No sub-classification | **Pa** |
| US-CJ-05 | Stop mixing and merge funds; sweep the CoinJoin account | AND (warns that funds merge); iOS `CoinJoinMoveFunds` (sweep, or drain into an asset lock to the shielded pool) | Build from the CoinJoin account (`add_funding`); (plat) drain helpers | **P** |
| US-CJ-06 | Keep Core-mixed and mobile-mixed coins usable after moving wallets | Core mixes on `m/44'/5'/0'/0/i` (same chain as receive); mobile mixes on `m/9'/5'/4'/0'/0/i` | Both accounts are derived and scanned (`AccountType::CoinJoin` has external and internal pools). Core-compatible rounds and `cj_salt` import: **M** | **Pa** |
| US-CJ-07 | **[v24]** Denomination promotion/demotion (10:1), `dsa` nFlags | QT/CLI v24 | none | **M** |
| US-CJ-08 | Serve mixing as a masternode (operator side) | dashd only | none (node) | **M** |

### 2.3 Masternode owner (US-MO)

| ID | Story | Apps today: how | rust-dashcore capability | Status |
|---|---|---|---|---|
| US-MO-01 | Derive owner, voting and payout keys from the wallet seed and view/export them (address, pubkey, WIF) | AND and iOS "Masternode Keys" tool (`masternode_keys/*`; iOS `DerivationPathKeysModel.swift`); DET (display bootstrap of voting/owner); QT/CLI `getnewaddress` | `AccountType::ProviderOwnerKeys` `m/9'/c'/3'/2'`, `ProviderVotingKeys` `m/9'/c'/3'/1'` (`account_type.rs:~493-530`). WIF export (`wallet/helper.rs:426`) | **P** |
| US-MO-02 | Register a regular masternode with collateral held in this wallet (`protx register` / `register_fund`; v24 QT wizard) | QT/CLI only. No mobile app or DET can register | `ProviderRegistrationPayload` encode/decode, `payload_collateral_string` (`dash/src/.../provider_registration.rs:99-180`), `base_payload_hash` (`special_transaction/mod.rs:462`), `inputs_hash` finalizer (`transaction_builder.rs:304-341`). **No ProRegTx flow** (choose collateral, sign collateral message, fee source, submit). rpc-client has only the client-side `protx register*` (`rpc-client/src/client.rs:1415-1607`) | **Pa** (types) / flow **M** |
| US-MO-03 | Register with external or hardware-wallet collateral: `register_prepare` → sign message with collateral key → `register_submit` | QT/CLI (`rpc/evo.cpp:946-980`); message is `payout\|reward\|owner\|voting\|SerializeHash(payload)` (`evo/providertx.cpp:375-395`), magic `"DarkCoin Signed Message:\n"` | Message-signing primitives **P**. Prepare/submit flow **M** | **M** |
| US-MO-04 | Update registrar: change operator key, voting key or payout (ProUpRegTx, owner-signed) | QT dialogs; CLI `protx update_registrar` | Payload **P** (`provider_update_registrar.rs`, with a `payload_sig` type TODO at `:57`). Builder/signing flow **M** | **Pa** |
| US-MO-05 | Revoke the operator (ProUpRevTx, signed by operator BLS) | QT; CLI `protx revoke` | Payload **P**. BLS signing: **M in-repo** (`crypto/src/bls.rs` is verify-only; `dash-pkc` could sign but isn't wired up) | **Pa** |
| US-MO-06 | "My masternodes": status, PoSe, collateral, last paid, payouts | AND tracks key usage (CURRENT / PREVIOUS / REVOKED + IP) by scanning ProTxs; iOS tracked masternodes; DET MN-002 (SPV list + Platform identity); QT masternode tab; CLI `protx list wallet`, `masternode status/winners/payments` | SML (`dash/src/sml/`) gives validity, service and keys. Wallet detects ProTxs referencing its keys (`key-wallet/src/transaction_checking/account_checker.rs:501-525`) and adds owner/voting/collateral to the filter match set. No protx-hash → state registry; no PoSe score in SML; (plat) has `TrackedMasternode` | **Pa** |
| US-MO-07 | Keep the 1000 / 4000 DASH collateral from being spent by accident | QT/Core auto-locks collateral | Only on the unmerged collateral-lock branch (see US-W-12) | **Pa** (unmerged) |
| US-MO-08 | Multi-payout ProTx (several payout scripts with bp shares) **[v24]** | CLI `payoutAddress` array (`rpc/evo.cpp:182-215`) | rust-dashcore ProRegTx stops at payload **v2** (`provider_registration.rs:123` `CURRENT_VERSION = 2`); Core v24 adds **`ProTxVersion::ExtAddr = 3`** (`dash/src/evo/types.h:19`) with `payouts` and extended net addresses. rpc-json modelling exists only on the `integration/v4.2-core-v24` branch | **M** (see §6 risk R-1) |
| US-MO-09 | Withdraw evonode Platform credits to Core; claim token rewards | DET MN-007 / MN-009; iOS | Platform state transitions (plat). AssetUnlock decode **P** | **P** (plat) |

### 2.4 Masternode operator (US-OP)

| ID | Story | Apps today: how | rust-dashcore capability | Status |
|---|---|---|---|---|
| US-OP-01 | Generate or derive the operator BLS key (legacy and basic scheme); `bls generate`, `bls fromsecret` | AND/iOS Masternode Keys (BLS pub, legacy pub, priv); CLI `bls *` | HD derivation `m/9'/c'/3'/3'` (`key-wallet/src/account/bls_account.rs:152-185`, `derivation_bls_bip32.rs`). Random keygen and `fromsecret` helpers are not exposed | **Pa** |
| US-OP-02 | Update service: IP/port, operator payout (ProUpServTx, BLS-signed); **[v24]** extended/non-IP addresses | QT dialog; CLI `protx update_service[_evo]`; iOS/(plat) `rs-platform-wallet/src/masternode` update-service | Payload **P**; `inputs_hash` finalizer tested with a dummy BLS sig (`transaction_builder.rs:2512-2580`); BLS signing **M** (TODO `provider_update_service.rs:309`). v3 ExtAddr **M** | **Pa** (works in (plat)) |
| US-OP-03 | Run the masternode: serve P2P, `mnauth`, DKG/quorum participation, IS/CL signing, PoSe, governance sync, CoinJoin server | dashd only | dash-spv is client-only: it ignores `getheaders` (`dash-spv/src/network/manager.rs:823-835`), doesn't serve `getdata`, has no LLMQ signing | **M** (node; the core of the program) |
| US-OP-04 | Check "is my node valid / PoSe-banned / ready" | CLI `masternode status`, `protx info` | SML entry validity **P**. Local operator self-status **M** | **Pa** |

### 2.5 Shared-masternode participant (US-SM) [v24]

Dash Core v24 adds split-collateral masternodes: ProRegTx v3 with a `CollateralShares` table (`dash/src/evo/providertx.h:31-127`) and new special tx types **10 ProviderDissolve, 11 ProviderUpdateShare, 12 ProviderUpdateSharedRegistrar** (`dash/src/primitives/transaction.h:39-41`). RPCs live at `rpc/evo.cpp:1229-1500,2161-2300`; QT has `sharedmn*.cpp`. **No mobile app or DET supports them.**

| ID | Story | Core mechanism | rust-dashcore | Status |
|---|---|---|---|---|
| US-SM-01 | Pool collateral with other owners into one masternode | `shared_register_prepare` (append shared collateral output + payload to the funding tx, return a consent hash) → each owner `shared_sign` → `shared_combine` → `signrawtransactionwithwallet` → `sendrawtransaction` | No v3 payload or share table, no consent hash (`MakeSharedRegConsentHash`), no multi-party flow | **M** |
| US-SM-02 | See my share, its reward script and payout split | `ShareListToJson`, `SplitAmountByShares` (sequential floor) | none | **M** |
| US-SM-03 | Change my share's reward script | `shared_update_share` → tx type 11 | none. **The decoder rejects type 11** (`special_transaction/mod.rs:378` `UnknownSpecialTransactionType`) | **M** |
| US-SM-04 | Update registrar of a shared masternode (operator/voting) | `shared_update_registrar_prepare` → type 12 | none (type 12 rejected) | **M** |
| US-SM-05 | Leave or dissolve: unilateral (early-period penalty, can be pre-signed and kept offline) or unanimous | `shared_dissolve`, `shared_dissolve_prepare` → type 10 | none (type 10 rejected) | **M** |
| US-SM-06 | Legacy "shared" model: owner/operator reward split | `operatorReward` 0–10000 bp + operator payout via ProUpServTx | Payload fields **P**; flows as in US-MO / US-OP | **Pa** |

### 2.6 Governance voter (US-GV)

| ID | Story | Apps today: how | rust-dashcore capability | Status |
|---|---|---|---|---|
| US-GV-01 | Browse proposals: status, yes/no/abstain tallies, budget, superblock timing | QT governance tab (`proposallist.cpp`, `proposalinfo.cpp`); CLI `gobject list/get/getcurrentvotes/count`, `getgovernanceinfo`, `getsuperblockbudget`. **AND, iOS and DET: none** (AND uses the superblock cycle only for an APY estimate) | No governance object or vote types anywhere (dash, rpc-json); no `govsync` / `govobj` / `govobjvote` P2P; no rpc-client `gobject` family | **M** (node: governance manager + object/vote sync + validation) |
| US-GV-02 | Vote with all my masternodes (funding / valid / delete / endorsed × yes / no / abstain) | QT `proposalvotedialog.cpp`; CLI `gobject vote-many`, `vote-alias` (iterates the MN list for voting keys in the wallet, `rpc/governance.cpp:516-547`) | Voting key derivation **P** (`ProviderVotingKeys`); SML lookup by voting key (plat) `masternodes_by_voting_key`. Vote struct, hash, ECDSA vote signing and relay **M** | **M** |
| US-GV-03 | Vote with an externally signed vote | CLI `voteraw` | none | **M** |
| US-GV-04 | Vote with an imported voting key (WIF) when the seed belongs to someone else | AND imports voting WIF for DPNS votes (`UsernameRequestsViewModel.kt:413-458`); DET loads masternodes by ProTxHash + voting WIF (`load_identity.rs:140-320`) | Imported-key account **M** | **M** |
| US-GV-05 | Platform contested-username (DPNS) votes with masternode voting keys: towards / abstain / lock, bulk, scheduled | AND, iOS, DET (DPN-003..015). Platform `MasternodeVoteTransition` signed with the voting key | Voting key + SML-by-voting-key **P** ((plat) `spv/runtime.rs:576`); state transition in rs-sdk | **P** (plat) |
| US-GV-06 | Get notified of upcoming votes or a superblock deadline | QT (none, beyond the tab); DET DPN-013 for DPNS | none for Core governance | **M** |

### 2.7 Proposal author (US-PA)

| ID | Story | Core mechanism | rust-dashcore | Status |
|---|---|---|---|---|
| US-PA-01 | Draft a proposal (name, URL, payment address, amount, start/end) and validate it | QT `proposalcreate.cpp`; CLI `gobject check`, `gobject deserialize` | none | **M** |
| US-PA-02 | Pay the 1 DASH collateral as an `OP_RETURN <govobj hash>` tx | CLI `gobject prepare`; `GOVERNANCE_PROPOSAL_FEE_TX = 1*COIN` (`governance/object.h:87`); stored as wallet record `g_object` | OP_RETURN output **P**. Governance object hash **M** | **Pa** |
| US-PA-03 | Submit after 6 confirmations, and resume if interrupted | CLI `gobject submit`, `gobject list-prepared` (`GOVERNANCE_FEE_CONFIRMATIONS=6`) | none; needs P2P `govobj` relay | **M** (node) |
| US-PA-04 | Track votes, passing threshold and superblock payment | CLI `gobject get`, `getsuperblockbudget`, `gobject list` | none | **M** (node) |

### 2.8 Evonode / Platform operator (US-EV)

| ID | Story | Apps today: how | rust-dashcore capability | Status |
|---|---|---|---|---|
| US-EV-01 | Register an evonode (4000 DASH, platformNodeID, platform P2P/HTTP ports) | CLI `protx register[_fund/_prepare]_evo` | ProReg v2 Evo fields **P** (`provider_registration.rs:115-118`); flow **M**; v3 ExtAddr (platform addresses as netinfo) **M** | **Pa** |
| US-EV-02 | Derive the platform node Ed25519 key and node ID; export for Tenderdash | AND/iOS (base64 `priv‖pub`, node ID = SHA256(pub)[:20]) | `ProviderPlatformKeys` `m/9'/c'/3'/4'` hardened SLIP-10 (`key-wallet/src/account/eddsa_account.rs:140`); `PlatformNodeId` (`crypto/src/eddsa.rs`). EdDSA signing **M** | **P** (derivation) |
| US-EV-03 | Withdraw credits, claim token rewards, manage node identity keys | DET MN-007..009; iOS | (plat) | **P** (plat) |
| US-EV-04 | Run the Core side that Platform (Drive / Tenderdash / DAPI) needs: quorum signing and verify, chainlock / islock feeds, `protx listdiff`, `getassetunlockstatuses`, etc. | Evonodes run dashd and expose RPC/ZMQ to Platform | rpc-client implements the client side (`quorum *`, `verifychainlock`, `verifyislock`, `getbestchainlock`, `getassetunlockstatuses`, `protx diff/listdiff`), which is a ready-made server schema. Server side **M** | **M** (node). Needs its own research track on Platform's dashd RPC/ZMQ contract |

### 2.9 Platform / DashPay user (mobile and DET; included for completeness)

These stories are served by `rs-platform-wallet` (plat) on top of rust-dashcore primitives. They are listed so the node program knows which primitives must stay stable.

| ID | Story | rust-dashcore primitive | Status |
|---|---|---|---|
| US-PL-01 | Create an identity funded by an asset lock (IS-lock or ChainLock proof); top up; invitations | `asset_lock_builder.rs:33-548` (Registration / TopUp / TopUpNotBound / Invitation / AddressTopUp / ShieldedTopUp); IS/CL proofs from dash-spv | **P** |
| US-PL-02 | Restore identities from the seed (identity gap scan) | Funding paths `m/9'/c'/5'/{1,2,3}'`. Auth keys `m/9'/c'/5'/0'/0'/id'/key'` are a **path helper only** (`dip9.rs:752`); no account or pool, so DET duplicates the derivation | **Pa** |
| US-PL-03 | DashPay contacts, payments to contacts, auto-accept | `DashpayReceivingFunds` / `DashpayExternalAccount` (DIP-14/15 256-bit) **P**. Contact-request crypto in (plat) | **P** / (plat) |
| US-PL-04 | Platform payment addresses (DIP-17), credit balances | `PlatformPayment` accounts (`managed_platform_account.rs`) | **P** |
| US-PL-05 | Usernames (DPNS), contested-name flow, marketplace; shielded pool; tokens | rs-sdk / (plat) | n/a (plat) |

### 2.10 Power user / importer (US-PU) (details in §3)

| ID | Story | Core / apps | rust-dashcore | Status |
|---|---|---|---|---|
| US-PU-01 | Import a Dash Core **legacy BDB** wallet.dat (non-HD keypool and/or Dash HD `hdchain`) | Core `loadwallet`, `migratewallet` [v24] | none | **M** |
| US-PU-02 | Import a Dash Core **descriptor (SQLite)** wallet.dat | Core v21+ (default from v24) | none | **M** |
| US-PU-03 | Import a `dumpwallet` text file | CLI `importwallet` | WIF parse **P**; imported-key account **M** | **M** |
| US-PU-04 | `importprivkey`, `importpubkey`, `importaddress`, `importmulti` | CLI (legacy wallets) | **M** | **M** |
| US-PU-05 | `importdescriptors`, `listdescriptors` | CLI (descriptor wallets) | No descriptor support | **M** |
| US-PU-06 | Import a mobile 12/24-word seed and see all funds (BIP44, legacy `m/0'`, CoinJoin, identity funding, masternode keys) | AND, iOS, DET | All paths derivable (§3.1) | **P** |
| US-PU-07 | Import an xprv or xpub | CLI `sethdseed` (WIF seed), descriptors | `from_extended_key` (`initialization.rs:346`), `from_xpub` (`:261`) | **P** / **Pa** |
| US-PU-08 | Import an Android wallet backup file (OpenSSL `enc`, AES-256-CBC, EVP_BytesToKey MD5 ×1024, base64 of the dashj protobuf or a WIF list) | AND (debug builds only) | none | **M** (low priority) |
| US-PU-09 | Import an Electrum-Dash wallet (`.csv` / `.json`) | CLI `importelectrumwallet` (Dash-specific, `wallet/rpc/backup.cpp:647`) | none | **M** (low priority) |
| US-PU-10 | Bootstrap the chain from local block files | `-loadblock`, `-reindex[-chainstate]`, copying `blocks/` | No block store or full validation | **M** (node) |
| US-PU-11 | Raw tx tools: create, decode, sign with key, send raw | CLI | Encode/decode **P** (dash); key-wallet-ffi raw-tx family **P**; broadcast **P** (`dash-spv/src/client/transactions.rs:32,68`) | **P** |
| US-PU-12 | dash-cli parity for wallet, masternode, governance and CoinJoin RPC | CLI | `rpc-json` has 158 types, about 90 with Serialize+Deserialize (reusable as server response schema). Missing families: `gobject` / `voteraw` / `getgovernanceinfo`, `coinjoin*`, `spork`, `signmessage`, v24 shared-masternode RPCs (only on the integration branch) | **Pa** (schema) / server **M** |

---

## 3. Import and migration

### 3.1 Derivation paths actually used in the wild (c = 5' mainnet, 1' testnet/devnet/regtest)

| Purpose | Path | AND (dashj) | iOS (Rust SDK) | DET | Dash Core | key-wallet `AccountType` |
|---|---|---|---|---|---|---|
| Legacy BIP32 | `m/0'/{0,1}/i` | **yes**: every wallet's default chain from `fromSeed` | yes | display | no | `Standard{BIP32Account}` = `m/acct'` |
| BIP44 funds | `m/44'/c'/0'/{0,1}/i` | yes (active) | yes | yes (acct 0) | **yes**. Legacy HD chain account 0; descriptor `pkh(xprv/44h/5h/0h/{0,1}/*)` | `Standard{BIP44Account}` |
| CoinJoin | `m/9'/c'/4'/0'/0/i` | yes. dashj issues mixing keys from the **receive subchain only** (`CoinJoinExtension.getUnusedKey → freshReceiveKey`) | read/sweep only | display | **descriptor wallets v23+**: inactive `pkh(xprv/9h/5h/4h/0h/0/*)` (recognize and spend only). Core's *own* mixing uses `m/44'/c'/0'/0/i` | `CoinJoin{index}` (ext + int pools, gap 100) |
| Masternode holdings | `m/9'/c'/3'/0'` | defined, unused | — | — | — | (none) |
| Provider voting | `m/9'/c'/3'/1'/i` | yes | yes | display | no (keys are just addresses) | `ProviderVotingKeys` |
| Provider owner | `m/9'/c'/3'/2'/i` | yes | yes | display | no | `ProviderOwnerKeys` |
| Provider operator (BLS) | `m/9'/c'/3'/3'/i` | yes (legacy + basic pub) | yes | — | no (`bls generate`) | `ProviderOperatorKeys` |
| Platform node (Ed25519) | `m/9'/c'/3'/4'/i'` (all hardened) | yes | yes | — | no | `ProviderPlatformKeys` |
| Identity auth (DIP-13) | `m/9'/c'/5'/0'/0'/id'/key'` | yes (id 0 only) | yes | yes (own derivation) | no | **path helper only** (`dip9.rs:752`) |
| Identity registration funding | `m/9'/c'/5'/1'/i` | yes | yes | yes | no | `IdentityRegistration` |
| Identity top-up | `m/9'/c'/5'/2'/[reg'/]i` | yes | yes | yes | no | `IdentityTopUp{…}` / `…NotBoundToIdentity` |
| Invitation funding | `m/9'/c'/5'/3'/i` | yes (the invite link carries the funding-key **WIF**) | yes | yes | no | `IdentityInvitation` |
| Asset-lock address / shielded top-up | `m/9'/c'/5'/{4,5}'/i` | — | yes | upstream | no | `AssetLockAddressTopUp`, `…ShieldedAddressTopUp` |
| DashPay contact (DIP-15) | `m/9'/c'/15'/acct'/(256-bit owner)/(256-bit contact)/i` | yes | yes | yes | no | `DashpayReceivingFunds` / `DashpayExternalAccount` |
| DashPay auto-accept | `m/9'/c'/16'/ts'` | — | yes | yes | no | dip9 constant |
| Platform payment (DIP-17) | `m/9'/c'/17'/acct'/class'/i` | — | yes | yes | no | `PlatformPayment` |

**Mnemonics.** AND supports 12/24 words in 9 auto-detected BIP39 languages, with **no passphrase**. iOS and DET also use no passphrase. Core supports 128–256 bits, **English only**, with an optional passphrase (≤256 chars) and PBKDF2-HMAC-SHA512 ×2048. key-wallet supports 12–24 words and 10 languages.

**Implications for a "restore any Dash seed" flow:**
1. Always scan BIP44 account 0 **and** legacy `m/0'` **and** CoinJoin `m/9'/c'/4'/0'` (both subchains), plus provider-key paths for ProTx recognition.
2. Offer an optional passphrase (US-W-03 gap).
3. Use a **large external lookahead (≥1000) on `m/44'/c'/0'/0`** when the seed came from Dash Core. Core's own CoinJoin consumes external-chain keys from a 1000-key pool, which can leave long gaps.
4. DashPay contact chains cannot be rediscovered from the seed alone. They need Platform contact requests, which are (plat) territory.

### 3.2 Importing a Dash Core wallet.dat

There is no Dash Core code to port for reading BDB without BDB. `migrate.cpp`/`BerkeleyRO` does not exist in Dash, and `migratewallet` still needs BDB compiled in (`wallet/rpc/wallet.cpp:684-688`). A Rust importer needs:

1. **Container detection** (`dash/src/wallet/db.cpp:97-134`):
   - BDB 4.8 btree: magic `0x00053162` at offset 12, logical db `"main"`.
   - SQLite: header `SQLite format 3\0`; `application_id` (offset 68) = BE32 network magic; table `main(key BLOB PK, value BLOB)`.
   - A **read-only BDB 4.8 btree page parser** must be written in Rust (leaf pages + overflow pages). This is the main engineering item. SQLite can use `rusqlite`.
2. **Record decoding** (`dash/src/wallet/walletdb.cpp:38-836`). The key is a serialized `(string type, …)`.

   | Records | Purpose |
   |---|---|
   | `key` / `ckey` / `mkey` / `keymeta` / `pool` / `cscript` / `watchs` / `watchmeta` | Loose keys, scripts, watch-only |
   | **`hdchain` / `chdchain` + `hdpubkey`** | Dash HD |
   | `walletdescriptor` / `walletdescriptorcache` / `lhcache` / **`walletdescriptorkey` / `walletdescriptorckey`** | Descriptors (with Dash's optional trailing **mnemonic + passphrase**) |
   | `activeexternalspk` / `activeinternalspk` | Active descriptors. Dash has no OutputType in the key |
   | `name` / `purpose` / `destdata` | Labels, `used`, `rr##` requests |
   | `tx` | Wallet txs (optional; can be rebuilt by rescan) |
   | `lockedutxo` | Locked coins |
   | **`g_object`** | Prepared governance proposals |
   | **`cj_salt` / `ps_salt` / `cj_pending_obs`** | CoinJoin state |
   | `platform_data` [v24] | — |
   | `flags` | `DESCRIPTORS = 1<<34`, `DISABLE_PRIVATE_KEYS = 1<<32`, `BLANK = 1<<33` |

3. **Legacy Dash HD (`CHDChain`)** (`wallet/hdchain.h:24-136`):
   - Serialized fields: `nVersion, id = SHA256d(seed), fCrypted, vchSeed, vchMnemonic, vchMnemonicPassphrase, map<acct, {ext, int counters}>`.
   - HD private keys are **not** stored. Only `hdpubkey` records (`extPubKey, chainId, account, change`) exist, so re-derive from the seed or mnemonic.
   - The chain counters give the exact lookahead needed.
4. **Decryption** (`wallet/crypter.cpp:14-116`):
   - Master key derivation: SHA512(pass‖salt), then SHA512 repeated `nDeriveIterations` times (≥25000). Key = first 32 bytes, IV = next 16.
   - Cipher: AES-256-CBC with PKCS7.
   - Per-secret IV: `pubkey.GetHash()[..16]` for keys and descriptor mnemonics; `chain.GetID()[..16]` for the `chdchain` seed, mnemonic and passphrase.
5. **Descriptor wallets** (available since v21; **default from v24**):
   - Parse `pkh()`, `combo()` and other descriptors (accept both `'` and `h` hardening).
   - The mnemonic, when present, sits inside the descriptor key records.
   - Recreate the inactive DIP-9 CoinJoin descriptor (v23+).
   - The importer needs a **descriptor parser/evaluator**, which rust-dashcore lacks (US-PU-05).
6. **What to migrate beyond keys:** labels and purposes, `lockedutxo`, prepared `g_object` proposals (for US-PA-03 resume), `cj_salt` (it affects Core's random-rounds coin flip in `IsFullyMixed`), and receive requests. CoinJoin rounds are **not stored**; recompute them from ancestry (Core `wallet/coinjoin.cpp:261-370`).
7. **Fallback paths** that need a Dash Core binary, so only acceptable as escape hatches:
   - `dash-wallet dump` produces `BITCOIN_CORE_WALLET_DUMP,1` / `format,…` / hex k,v lines / `checksum,SHA256` (`wallet/dump.cpp:20-95`). Its records are the same as above, without the BDB parsing problem.
   - `dumphdinfo` returns the mnemonic; re-enter it as a seed.

### 3.3 Importing dumped keys or descriptors

- **`dumpwallet` text** (`wallet/rpc/backup.cpp:1006-1095`). Header comments carry the mnemonic, passphrase, HD seed, xprv and chain counters. Body lines look like `<WIF> <ISO8601> [label=…|reserve=1|change=1] # addr=… hdkeypath=m/44'/5'/0'/0/i`, plus `<hex script> … script=1` lines.
  - Core's `importwallet` **skips the comment lines** (mnemonic and seed are not re-imported).
  - A Rust importer should parse the comments: recreate the HD wallet from the mnemonic or seed, then add loose WIFs as imported keys.
  - Requires: an imported-key account type (**M**), and WIF → P2PKH watch plus signing (**Pa**).
- **`importprivkey` / `importaddress` / `importpubkey` / `importmulti`**: the same imported-key / imported-script store (**M**), plus a rescan from a timestamp (US-W-15 **Pa**).
- **`importdescriptors`** (`desc, active, range, next_index, timestamp, internal, label`; no mnemonic field) and **`listdescriptors true`** (adds `mnemonic`, `mnemonicpassphrase` and a `coinjoin` flag): needs a descriptor module (**M**). Supporting `pkh()`, `combo()`, `sh(multi())` and raw `addr()` covers Dash.
- **Electrum-Dash** (`importelectrumwallet`) and **BIP38** paper wallets: BIP38 was deliberately removed from key-wallet (#1111). Treat both as low-priority sweep-only importers.

### 3.4 Importing mobile wallets

- **Seed phrase**: fully supported by key-wallet paths (§3.1). Default account creation should include `BIP32Account{0}` (legacy `m/0'`, which every Android wallet has), BIP44 0, CoinJoin 0 (both subchains) and the provider key accounts.
- **Birth date**: Android stores an optional user-supplied creation date; the default `EARLIEST_HD_SEED_CREATION_TIME` is 1427610960 (2015-03-29). Map it to a birth height through checkpoints.
- **Masternode keys**: Android/iOS hand out operator/owner/voting/platform keys by index from the same seed. A restored wallet finds them by scanning ProTxs for those keys (`account_checker.rs:501-525`). Only voting keys can be *imported* as WIF in AND.
- **Not portable from mobile**: tx metadata (AND stores it optionally, encrypted, on Platform), address-book labels, CoinJoin mode, and the dashj protobuf itself (US-PU-08).

### 3.5 Importing a bootstrap of blocks

- **Dash Core facts:**
  - `-loadblock=<file>` replays `blkNNNNN.dat`-format files: `[4-byte netmagic][LE u32 size][block]` records (`node/blockstorage.cpp:633-640,898-932`).
  - `bootstrap.dat` is **not** handled automatically; use `-loadblock`.
  - `-reindex` / `-reindex-chainstate` rebuild from `blocks/`.
  - `dumptxoutset` exists, but **`loadtxoutset` does not**: assumeutxo params are empty and the snapshot "carries no Dash payload" (no MN list; `validation.cpp:6372-6386`). Assumeutxo is therefore not usable for Dash today.
  - Dash-specific state that **cannot be imported, only rebuilt by replay**: `evodb/` (deterministic MN list), `llmq/{isdb,dkgdb,quorumdb,recsigdb}`, plus flat caches `mncache.dat`, `governance.dat`, `netfulfilled.dat` and sporks. Governance state is re-synced from peers, not from blocks.
- **For the Rust node:**
  1. Support `-loadblock`-style ingestion of blk files. It is a trivial format and lets users reuse an existing dashd `blocks/` dir without its LevelDB index.
  2. Rebuild the MN list, quorums and IS/CL state by replay.
  3. Optionally import a dashd datadir's LevelDB block index to skip header re-validation.
- **For SPV mode:** Android already ships `mnlistdiff.dat` + `qrinfo.dat` + checkpoints as a bootstrap (`BlockchainServiceImpl.kt:1074-1080`). dash-spv uses checkpoints + QRInfo. A signed "SPV bootstrap bundle" (headers + filter headers + MN-list snapshot) would serve the desktop app's light mode.

---

## 4. How DET gets chain data, and what still needs dashd

**Summary: DET is already SPV-only. It does not depend on dashd except for regtest/devnet peers and block mining.** PR #860 (`1397b434`, the platform-wallet rewrite) removed:
- the RPC/SPV/Auto backend selector (v34 migration pins SPV; v38 drops the column, `src/database/initialization.rs:242-330`);
- the ZMQ listener;
- the dash-qt launcher;
- the RPC wallet backend.

`docs/user-stories.md` NET-003 (Dash-Qt path) and NET-009 (ZMQ toggle) are stale.

**Chain data paths:**
- **Core chain:** `platform-wallet::SpvRuntime` wraps `DashSpvClient<WalletManager<PlatformWalletInfo>, PeerNetworkManager, DiskStorageManager>`.
  - DET's config (`src/wallet_backend/mod.rs:3064-3076`): `ValidationMode::Full`, `MempoolStrategy::BloomFilter`, start height 0. Peers: DNS seeds on mainnet/testnet; `core_host:20001` (devnet) or `:19899` (regtest).
  - Events reach the UI through `EventBridge` (`src/wallet_backend/event_bridge.rs:176-315`).
  - Broadcasts go over P2P (`SpvBroadcaster`).
- **Quorum keys for Platform proof verification:** `spv().get_quorum_public_key()` (`mod.rs:2454-2468`, `context_provider.rs:125-171`).
- **Masternode list:** `spv().masternode_list_summaries()`. It feeds masternode status and DPNS-vote eligibility (`context/dpns_vote_attention.rs:142-147`).
- **Platform:** dash-sdk gRPC (DAPI) with proofs verified via SPV quorum keys. Endpoint discovery: `quorums.{mainnet,testnet}.networks.dash.org/masternodes` (`src/backend_task/dapi_discovery.rs:91-103`).
- **Withdrawal / asset-unlock status:** Platform Withdrawals contract documents, **not** `getassetunlockstatuses`.

**Every remaining Core RPC call (there are only three call sites):**

| RPC | Where | Feature | Needs a node? |
|---|---|---|---|
| `getbestchainlock` | `src/backend_task/platform_info.rs:608-617` | Tools → Platform Info: "Core chain-lock height" | No. SPV already holds the best ChainLock; the fallback is trivial and currently missing (shows `None`) |
| `getbestchainlock` | `src/backend_task/core/mod.rs:111-162,336-383` | Network screen chain-lock poll | Dead code (`any_rpc_backend()` returns `false`, `ui/network_chooser_screen.rs:1326-1329`) |
| `generatetoaddress` | `src/backend_task/core/mod.rs:307-331` | "Mine blocks" (regtest/devnet only) | **Yes**, inherently |

**DET features that are missing (not merely needing RPC), and the upstream gaps behind them.** These are the asks for the node/library:

| DET gap | Blocking library gap | Story |
|---|---|---|
| Single-key (WIF) wallets show no balance and cannot send (WAL-031, SND-002) | Watch-only / imported-key wallet registration: key-wallet imported-key account (**M**); (plat) `register_wallet` is private | US-W-14, US-W-18, US-GV-04 |
| Hand-built payments; no fee override; fixed selection strategy | High-level send API with fee policy and subtract-fee | US-W-07, US-W-08 |
| Own asset-lock max-amount probe and duplicated `DEFAULT_FEE_PER_KB` (`payments.rs:35-38,105-630`) | "Max sendable for asset lock" API | US-PL-01 |
| Own identity-funding-account registrar (`identity_ops.rs:810-1006`, "UPSTREAM GAP") and own DIP-13 auth-key derivation | Identity-auth account type with pool and gap scan | US-PL-02 |
| Own fee math (`model/fee_estimation.rs:1017-1086`) | Fee estimator; fix `FeeRate::economy()` = 500 | US-W-08 |
| No ProTx registration, update or revoke; no operator BLS; no shared masternodes | ProTx builders + BLS / EdDSA signing + v3 / types 10–12 | US-MO-02..08, US-OP-02, US-SM-* |
| No CoinJoin | CoinJoin client + P2P messages | US-CJ-* |
| No Core governance (vote or proposals) | Governance types, P2P sync, signing | US-GV-01..03, US-PA-* |

**What the Rust node must serve so DET or the new desktop app can drop dashd entirely:**
1. **P2P services for light clients.** The two mobile stacks need *different* ones:
   - **BIP157/158** `getcfilters` / `getcfheaders` / `getcfcheckpt` (NODE_COMPACT_FILTERS) for dash-spv, which iOS, DET and the future Android SDK path use.
   - **BIP37** bloom filtering (NODE_BLOOM, `filterload` / `merkleblock`) for dashj Android (`MempoolStrategy::BloomFilter` is also DET's mempool mode).
   - `headers2` (compressed headers; both stacks use them).
   - `getmnlistd` / `mnlistdiff` and `getqrinfo` / `qrinfo` serving.
   - `clsig` / `isdlock` relay.
   - Mempool `inv` / `tx` relay and `getdata` for blocks.
   - addrv2.

   dash-spv today serves none of this (`network/manager.rs:823-835` ignores `getheaders`).
2. **Regtest/devnet:** be the local P2P peer and implement `generatetoaddress` (DET "Mine blocks"; also needed for the integration-test harness that currently uses dashd).
3. **Optional RPC for parity:** `getbestchainlock`. Then the wallet, masternode, governance and CoinJoin RPCs from §2 if the desktop app talks to the node over RPC rather than linking the library in-process.
4. **Platform (evonode) contract** (US-EV-04): the dashd RPC/ZMQ surface that Drive / Tenderdash / DAPI consume. This needs its own audit; rpc-client's `quorum*` / `verify*` / `protx diff` / `getassetunlockstatuses` methods are the likely list.

---

## 5. Gap list, prioritized for the node program

| # | Gap | Stories unblocked | Where it belongs |
|---|---|---|---|
| G-1 | **Core v24 transaction compatibility**: ProTx payload v3 (`ExtAddr`: extended net addresses, multi-payout, collateral shares) and special tx types 10 / 11 / 12. Today the decoder returns `UnknownSpecialTransactionType` for 10–12 (`dash/src/.../special_transaction/mod.rs:378`) and `ProviderRegistrationPayload` stops at v2 | Everything once v24 activates (block/tx decoding); US-MO-08, US-SM-*, US-OP-02, US-EV-01 | `dash` crate (SML already has `ExtNetInfo`; reuse it in ProTx payloads), rpc-json (merge `integration/v4.2-core-v24`) |
| G-2 | Wallet encryption at rest + lock/unlock + encrypted backup format | US-W-05, US-W-22, wallet.dat import of encrypted wallets | key-wallet (+ manager) |
| G-3 | Imported keys / scripts / addresses and seedless watch-only wallets, with rescan-from-height | US-W-14, US-W-15, US-W-18, US-GV-04, US-PU-03/04, DET WAL-031 | key-wallet, key-wallet-manager, dash-spv (UTXO-scan path for a full node) |
| G-4 | BIP39 passphrase end to end (wallet type, FFI, persistence) | US-W-03, Core import | key-wallet, key-wallet-ffi |
| G-5 | ProTx flows: register / fund / prepare-submit / update_service / update_registrar / revoke, with BLS + EdDSA signing; merge collateral-lock branch; "my masternodes" registry | US-MO-*, US-OP-01/02, US-EV-01 | key-wallet (builders), crypto (BLS sign via dash-pkc), key-wallet-manager |
| G-6 | Governance: object and vote types, hashing, signing, P2P `govsync` / `govobj` / `govobjvote`, governance manager with superblock tracking; RPC `gobject*` / `voteraw` / `getgovernanceinfo` | US-GV-01..06, US-PA-01..04 | dash (types, messages), node (manager), rpc |
| G-7 | CoinJoin client: denoms, collateral, sessions, queue, P2P `ds*`, Core-compatible rounds and `cj_salt`, promotion/demotion [v24]; plus the masternode server side | US-CJ-01..08 | new crate (e.g. `coinjoin`), dash (messages), node |
| G-8 | Message signing API, UTXO lock API, fee estimation (and fix `economy()` = 500), BIP21 parse/build, PSBT, descriptors | US-W-06/08/12/13/23, US-PU-05 | key-wallet, dash |
| G-9 | wallet.dat importer: BDB 4.8 read-only btree, SQLite, CHDChain, descriptor-key mnemonic, Core crypter, record migration; dumpwallet parser | US-PU-01..03 | new crate / tool |
| G-10 | Light-client serving (BIP157/158, BIP37, headers2, mnlistdiff / qrinfo, clsig / isdlock), `-loadblock` ingestion, regtest mining | §4 node-serving list, US-PU-10 | node |
| G-11 | Identity-auth (DIP-13) account type with pool and gap scan; asset-lock "max" API | US-PL-02, DET reimplementations | key-wallet ((plat) consumers) |

---

## 6. Risks, discrepancies and open questions

- **R-1 (high): v24 decode breakage.** When `DEPLOYMENT_V24` activates (`dash/src/consensus/params.h:44`), blocks carrying type 10–12 txs or v3 ProTxs may fail to decode in rust-dashcore. That would hit dash-spv block download for wallets that match such txs, and possibly MN-list diffs. This affects iOS and DET in production, not just the node program.
  - The `integration/v4.2-core-v24` branch only patches rpc-json.
  - MN-list diffs are probably safe: SML entries already decode v24 extended net info (`MasternodeNetInfo::Extended(ExtNetInfo)`, `dash/src/sml/masternode_list_entry/mod.rs:50-83`). The gap is the **transaction payloads** themselves.
  - To verify: how dash-spv reacts to a matched block it cannot decode.
- **R-2: pin drift.** Platform pins a rust-dashcore commit that is not on `dev` (`40268cc0` on `integration/v4.2-core-v24`), and both platform and DET are 88 commits behind `dev`, including the breaking #1115. Any node-program API change reaches the apps only after platform re-pins.
- **R-3: mobile CoinJoin is effectively legacy.** iOS removed it, and only dashj Android still mixes. The desktop app would be the first Rust CoinJoin client, so there is no in-ecosystem Rust reference implementation; Dash Core `src/coinjoin/` is the spec.
- **R-4: the wallet API surface is split.** Consumers use `rs-platform-wallet` (Platform repo) as the wallet facade; key-wallet / key-wallet-manager are underneath. The node program must decide whether the desktop app links `rs-platform-wallet` (with Platform features) or a new node-side wallet service, and keep both on one key-wallet.
- **R-5: interop detail to confirm.** Core's descriptor CoinJoin path covers only `m/9'/5'/4'/0'/0/*`. dashj issues CoinJoin keys from the receive chain (so Core sees them), and key-wallet scans both subchains. Confirm dashj never puts collateral or change on `/1/*`, which Core would miss.
- **Stale docs to ignore:**
  - DET `docs/user-stories.md` NET-003 / NET-009.
  - The DET comment at `src/wallet_backend/payments.rs:128-136` (old pins).
  - The dash-spv ARCHITECTURE.md claim that ChainLock verification is "remaining work" (it is implemented).
- **Not verified:**
  - dashj derivation constants at the exact pinned tag 22.0.5 (read from dashj `master`).
  - The iOS app's internals beyond the files sampled via `gh`.
  - Android's new `rs-unified-sdk-jni` path is not yet in the shipping app.
