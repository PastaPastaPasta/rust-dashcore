# WL1: key-wallet extensions (passphrase, Core mnemonic compatibility, imported/watch-only/seedless, encryption at rest, message signing)

| | |
|---|---|
| Status | D design **rev8 — ACCEPTED** (manager delta check of Opus-h r5, 2026-10-14; answers Sol r1–r3, §10–§10.2; Opus-h r1, §10.3; Opus-h r2, §10.4; Opus-h r3: M1, m1–m7, n1–n6, plus the WL1-1 rulings W1-3 and W1-13, §10.5; Opus-h r4: M1, m1, m2, n1–n5, simplified toward Core per the manager's rulings, §10.6; Opus-h r5: M1, m1, m2, n1–n6, §10.7). WL1-1 is implemented on `node/wl1-1-codec`; WL1-2 follows on `node/wl1-2` |
| Date | 2026-10-14 |
| Author | WL1 designer (Opus 5.5) |
| Task | Additive key-wallet / key-wallet-manager changes: BIP39 passphrase end to end, `MnemonicCompat::DashCore`, imported-key / watch-only / seedless wallets and accounts, encryption at rest (argon2id + XChaCha20-Poly1305), message signing |
| PRD | FR-WAL-01…04, FR-WAL-15; US-WAL-02, -06, -10, -15 |
| Binding inputs | A1 §2.20 items 1, 2, 3, 8; A1 §2.24 (FFI untouched, bincode layouts pinned); DEC-01, DEC-02, DEC-11, DEC-13, DEC-82; R5 G-2, G-3, G-4, G-8 |
| Core read at | `~/workspace/dash` develop **`15db47875002cc09d8a21e1e35fed4a74aaf4e4f`**. Every Core citation below is at that commit, paths under `src/` unless they start with `test/`. **Identity at the program pin `v24.0.0-rc.3` (`0364ed626e`), checked with `git diff v24.0.0-rc.3 15db47875`:** these source files are byte-identical: `wallet/bip39.*`, `wallet/bip39_english.h`, `wallet/hdchain.*`, `wallet/crypter.*`, `util/message.*`, `util/strencodings.cpp`, `key.cpp`, `pubkey.cpp`, `wallet/rpc/{backup,encrypt,signmessage,wallet}.cpp`, `rpc/signmessage.cpp`, `wallet/scriptpubkeyman.cpp`, `src/test/data/bip39_vectors.json`, `wallet/test/bip39_tests.cpp`. So are the functional tests whose assertions §6 transcribes: `wallet_upgradetohd.py`, `wallet_mnemonicbits.py`, `wallet_hd.py`, `wallet_encryption.py`, `wallet_createwallet.py`, `wallet_signmessagewithaddress.py`, `rpc_signmessagewithprivkey.py`, `wallet_watchonly.py`, `wallet_import_rescan.py`, `wallet_import_with_label.py`, `wallet_importmulti.py`. **Not identical:** `wallet/wallet.cpp` (the diff only touches coinbase abandonment and `MarkConflicted`; lines after 1091 shift by a few dozen); `test/util_tests.cpp` (unrelated edits; the `message_sign`/`message_verify`/`message_hash` sections are identical and sit one line earlier at rc.3); `test/functional/wallet_migration.py` (develop adds `test_conflict_txs`, `:667-719`; WL1 transcribes nothing from that file) |
| rust-dashcore read at | `node/integration` `af5aae51e` (key-wallet, key-wallet-manager, key-wallet-ffi, dash, dash-spv, dash-spv-ffi, test-vectors) |
| Desktop read at | dashwallet-desktop `main` `ef4da23a` (`docs/HANDOFF.md`, `rust/crates/{dw-compat,dw-vault,dw-message,dw-engine}`) |

---

## 0. Summary

1. **Nothing existing changes meaning.** Every change is additive: new modules, new constructors, enum variants appended at the end (bincode variant indices are a compat contract, `key-wallet/src/dip9.rs:33`), and no new fields in any existing bincode struct. Every wallet that can be built today serialises byte-for-byte as it does now (new golden tests pin it, §2.8). The C ABI keeps every existing function and value. The only FFI change is appended enum values for the two imported account kinds (§2.10).
2. **Passphrase end to end.** A new `WalletType::MnemonicWithPassphrase` carries the mnemonic, the passphrase and the derivation mode. New constructors on `Wallet` and `WalletManager` take a passphrase and a `MnemonicCompat`. The empty-passphrase strict-BIP39 case still produces today's `WalletType::Mnemonic`, so its bytes and wallet ID are unchanged.
3. **`MnemonicCompat::DashCore`** is a verbatim port of `CMnemonic::Check`/`ToSeed`: the weak checksum mask, no NFKD, salt cut at 256 bytes. It lives in a new `DashCoreMnemonic` type, because a phrase Core accepts may be one `bip39::Mnemonic` cannot even hold. The node uses it at every entry point (D3), and strict BIP39 stays the library default.
4. **Imported, watch-only and seedless.** There are two new account kinds, `AccountType::ImportedKeys` (spendable) and `AccountType::ImportedWatchOnly`. Each is a non-HD script set with no xpub. A new `WalletType::Seedless` is a private-key-capable wallet with no HD seed (Core `blank=true`, or a legacy non-HD wallet). Watch-only xpub wallets (`WalletType::WatchOnly`) already exist and are unchanged.
5. **Encryption at rest** is our own format (DEC-11), not Core's crypter. A random 32-byte master key is wrapped under an argon2id KEK. Each secret is sealed under the master key with XChaCha20-Poly1305, a random 24-byte nonce and AAD bound to the wallet, record kind and format version. A passphrase change re-wraps only the master key, as Core does. `WalletType::Encrypted` holds only ciphertext and public metadata. Lock, unlock, mixing-only unlock and `upgradetohd` on an encrypted blank wallet follow Core's semantics, and timers stay with the caller. Readers apply a caller-selected pre-authentication KDF budget, mobile-safe by default (D28), and refuse any format they do not fully understand (D30). The KDF floor applies on write only; a reader upgrades weaker files on unlock (D44). Imported private keys of an encrypted wallet exist in memory only as sealed records (D41). Every unlock re-derives the public data from the secrets and refuses a mismatch (D43); while locked, public metadata is unauthenticated, as in Core.
6. **Message signing** is a new `key_wallet::message` module, Core-exact in both directions: `MessageHash` with the `"DarkCoin Signed Message:\n"` magic, 65-byte compact signatures with Core's header arithmetic, a port of Core's strict `DecodeBase64`, and `MessageVerificationResult` mirrored one to one.
7. **Consumers.** dash-spv compiles unchanged except one additive mempool-filter input for raw imported scripts (D49), and both FFI crates change only by the appended values and their eight mapping arms (§2.10). **Downstream:** Platform's `rs-platform-wallet-ffi` matches `AccountType` exhaustively, so it needs arms for the two new kinds when it upgrades (§4.4); that source break is accepted under 0.x, and no public enum becomes `#[non_exhaustive]` (D50). The new crypto dependencies sit behind a default-off `encryption` feature, so dash-spv's dependency graph and benches are unaffected. The node wallet (WL2a/b) and WL3 import consume everything. The desktop reaches it only after upstreaming (D21). **FFI consumers (iOS/Android) get none of the new capabilities in WL1:** no passphrase constructor, no encryption or unlock calls, no message signing, and a locked wallet is indistinguishable from other `InvalidState` errors. That is a consumer limit, assigned to an explicit follow-up (§2.10, D29). Two further 0.x source breaks are listed in §4.4: three required `WalletInfoInterface` methods (D78) and a trailing `change_address` argument on the three wallet-level build entry points (D84).
8. **Seven slices** (§7), each independently mergeable with its own test bar. 97 D-rows (§8).

---

## 1. Survey

### 1.1 key-wallet and key-wallet-manager today

**Mnemonic and seed**
- `key_wallet::Mnemonic` wraps `bip39::Mnemonic` 2.2.2, which is strict BIP39 with all 10 languages (`key-wallet/src/mnemonic.rs:117-122`, `key-wallet/Cargo.toml:39`).
- `from_phrase` tries English first, then the other languages in `Language::ALL` order (`mnemonic.rs:202-218`).
- `to_seed(&self, passphrase: &str) -> [u8; 64]` delegates to `bip39::Mnemonic::to_seed`. That function NFKD-normalises the passphrase and runs PBKDF2-HMAC-SHA512 for 2048 rounds (`mnemonic.rs:267-271`).
- **The empty passphrase is hard-coded** at `wallet/initialization.rs:108` (`new_random`), `:225` (`from_mnemonic`) and `wallet/helper.rs:115` (`wallet_seed_bytes`). `Wallet::from_mnemonic` has no passphrase parameter (`initialization.rs:220-224`), and neither does any `WalletManager` constructor (`key-wallet-manager/src/lib.rs:274, 332, 397`).
- A passphrase already reaches the account-derivation helpers (`account/derivation.rs:202-224`, `Option<&str>`).
- The FFI has `mnemonic_to_seed(mnemonic, passphrase, …)` (`key-wallet-ffi/src/mnemonic.rs:190`), but no wallet constructor that takes a passphrase.

**Wallet types**
- `WalletType` (`wallet/mod.rs:33-66`, bincode variant index in brackets) has five variants:
  - `Mnemonic {mnemonic, root_extended_private_key}` [0], documented as "without passphrase";
  - `Seed {seed, root_extended_private_key}` [1];
  - `ExtendedPrivKey(RootExtendedPrivKey)` [2];
  - `ExternalSignable` [3];
  - `WatchOnly` [4].
- `Wallet {network, wallet_id, wallet_type, accounts: AccountCollection}` (`wallet/mod.rs:73-85`).
- The wallet ID is `sha256(root_pubkey ‖ chain_code ‖ "N" ‖ net_byte)`, with known-answer tests at `wallet/mod.rs:710-740`. `WatchOnly` and `ExternalSignable` keep the stored ID (`:154-164`).
- **Accounts.** `AccountType` has 15 variants (`account/account_type.rs:34-105`). `Account` is xpub-based and immutable, and `is_watch_only` marks xpub-only accounts (`account/mod.rs:54-68, 95, 111, 133`).
- **Watch-only today** is a whole-wallet property, `WalletType::WatchOnly` with per-account xpubs. There is no non-HD script or key container, so `importprivkey`/`importaddress`/`importpubkey` have nowhere to go (R5 US-W-14).
- **Mutable state.** The managed side is `ManagedWalletInfo` / `ManagedAccountCollection` / `ManagedAccountType` with `AddressPool`s (`managed_account/managed_account_collection.rs:40-75`, `managed_account_type.rs:15-80`).
  - `ManagedAccountCollection` and `ManagedWalletInfo` derive serde only; `ManagedAccountType` also derives bincode.
  - Transactions are routed through `transaction_checking::{transaction_router::AccountTypeToCheck, account_checker::CoreAccountTypeMatch}`.

**Persistence**
- `Wallet::backup()` is `bincode::encode_to_vec(self, standard())`. It has no magic and no version, and the mnemonic phrase is in clear (`wallet/backup.rs:28-54`, `mnemonic.rs:123-146`).
- `WalletManager::create_wallet_from_mnemonic_return_serialized_bytes` and `import_wallet_from_bytes` use the same raw bincode (`key-wallet-manager/src/lib.rs:332, 535-541`), and the FFI exposes both (`key-wallet-ffi/src/wallet_manager.rs:195, 288`).
- No test pins the byte layout.

**Errors**
- `key_wallet::Error` is a hand-written `Display` enum deriving `Clone, Eq`, not `#[non_exhaustive]`, with no thiserror (`key-wallet/src/error.rs:10-103`).
- `key_wallet_manager::WalletError` is the same (`key-wallet-manager/src/error.rs:9-32`).
- `key-wallet-ffi/src/error.rs:242` matches `key_wallet::Error` exhaustively, so a new variant there is an FFI source change.

**Secret hygiene**
- **Zeroization today.** `Mnemonic` is `ZeroizeOnDrop`; `Wallet` has a manual `Zeroize` + `Drop` (`wallet/mod.rs:234-238`); `RootExtendedPrivKey`/`ExtendedPrivKey` have `Zeroize` + `Drop` (`root_extended_keys.rs:24-35`). `WalletType` derives `Zeroize` but has **no `Drop`** (`wallet/mod.rs:33`), and `Seed` is `Copy` (`seed.rs:16`), so a `Seed`-variant `WalletType` dropped on its own is not wiped.
- **`Debug` today.** bip39 2.2.2's `Mnemonic` `Debug` prints only the language (`bip39-2.2.2/src/lib.rs:658-665`), so `{:?}` of a wallet prints no word. `RootExtendedPrivKey` derives `Debug` (`root_extended_keys.rs:17`). `Seed` (`seed.rs:106-111`) and `ExtendedPrivKey` (`bip32.rs:465-475`) redact. No consumer reads any of these formats.
- **`Display` today.** `Mnemonic` prints the phrase (`mnemonic.rs:433-437`), `ExtendedPrivKey` the xprv (`bip32.rs:1870`), `Seed` 8 of its bytes (`seed.rs:113-118`). WL1 leaves these as they are (D40).
- possible security note: see manager.

**Dependencies**
- **Encryption code.** There is none in key-wallet. `grep -rniE "encrypt|aes|chacha|argon|kdf"` finds only DIP9 path constants.
- **Crates already present.** `zeroize 1.8`, `bip39` (zeroize), `unicode-normalization` and `secp256k1` (recovery) are already dependencies.
- **Crates in the workspace lockfile.** `chacha20poly1305 0.11.0` (via `dash-p2p-transport`, BIP324), `thiserror 2.0.21`, `subtle 2.6.1` and `base64 0.22`.
- **argon2.** It is not in the workspace.

**Message signing**
- `dash/src/sign_message.rs` provides:
  - `DASH_SIGNED_MSG_PREFIX = b"\x19DarkCoin Signed Message:\n"` (`:31`);
  - `signed_msg_hash(&str)` (`:219-226`, UTF-8 only);
  - `MessageSignature::{from_slice, recover_pubkey, is_signed_by_address, from_base64}` (`:103-215`).
- **Gaps against Core.**
  - There is no signing function.
  - `from_slice` rejects header bytes below 27 (`:139-143`), which Core accepts (§1.2.4).
  - `is_signed_by_address` returns `Err` for non-P2PKH, where Core returns `ERR_ADDRESS_NO_KEY`.
  - Base64 goes through the `base64` crate, not Core's decoder.
  - key-wallet has no message API (R5 US-W-13).

**Consumers inside rust-dashcore**
- **dash-spv** is generic over `W: WalletInterface` (`dash-spv/src/client/core.rs:108` and the sync managers). It names `WalletManager` only in `client/mod.rs:48` and never matches on `WalletType` or `AccountType`.
- **dash-spv-ffi.** `src/callbacks.rs` matches `WalletEvent` exhaustively and converts `AccountType` through key-wallet-ffi's `FFIAccountType::from` (`key-wallet-ffi/src/managed_account.rs:769`).
- **key-wallet-ffi** mirrors `AccountType` as `#[repr(C)] FFIAccountKind` with explicit values 0–15 (`key-wallet-ffi/src/types.rs:204-239`). It mirrors the transaction-checking enums in `src/transaction_checking.rs`, and it does not mirror `WalletType`.
- **Benches.** `key-wallet/benches/address_generation.rs` and `key-wallet-manager/benches/filter_scan.rs`. dash-spv's `storage` bench is not wallet-related.

**Vectors already vendored.** `test-vectors/core/0364ed626ec2/src/test/data/bip39_vectors.json` (24 Trezor vectors, provenance `PROVENANCE.md:38`) with the loader `dash_core_vectors::cases::load_bip39` (`test-vectors/src/cases.rs:481-495`).

### 1.2 Dash Core

#### 1.2.1 BIP39 (`wallet/bip39.cpp`, `wallet/bip39_english.h`)

- **Word list.** English only: `wordlist[]` (`bip39_english.h:27`), 2048 words, `nullptr`-terminated (`:2076`). Core has no other language.
- **`Generate(strength)`** (`bip39.cpp:33-42`): returns an empty string unless `strength % 32 == 0` and `128 ≤ strength ≤ 256`. It draws 32 strong random bytes and calls `FromData(data, strength/8)`.
- **`FromData`** (`:45-76`) is standard BIP39: SHA256 checksum byte appended, 11-bit words, single spaces.
- **`Check`** (`:78-138`), verbatim semantics:
  - **Empty** input → false (`:80-82`).
  - **Word count** = number of `' '` + 1. It must be divisible by 3 and between 12 and 24 (`:84-95`). There is no trimming, so leading, trailing or double spaces produce an empty word, which then fails the lookup.
  - **Words** are case-sensitive exact matches by linear scan. A word of 10 or more characters → false (`:105-107`; a 9-character word passes this guard and then fails the lookup, with the same verdict). An unknown word → false (`:113-115`). A bit count other than `11 × words` → false (`:128-130`).
  - **Checksum** (`:131-137`):
    ```cpp
    bits[32] = bits[nWordCount * 4 / 3];
    CSHA256().Write(bits.data(), nWordCount * 4 / 3).Finalize(bits.data());
    const char checksum_length = nWordCount / 3;
    const char mask = (2 ^ checksum_length) << (8 - checksum_length);
    return (bits[0] & mask) == (bits[32] & mask);
    ```
    `^` is XOR, so the mask is not the top `cs` bits. It is a strict subset of the correct mask. Core therefore accepts every valid BIP39 phrase, plus phrases with a wrong checksum in the unchecked bits:

    | Words | `cs` | Core mask | Correct mask | Bits checked |
    |---|---|---|---|---|
    | 12 | 4 | `0x60` | `0xF0` | 2 of 4 |
    | 15 | 5 | `0x38` | `0xF8` | 3 of 5 |
    | 18 | 6 | `0x10` | `0xFC` | 1 of 6 |
    | 21 | 7 | `0x0A` | `0xFE` | 2 of 7 |
    | 24 | 8 | `0x0A` | `0xFF` | 2 of 8 |

    This is the "weak checksum" the PRD already lists (US-WAL-02). It is a compatibility rule: WL1 must accept exactly the same set.
- **`ToSeed(mnemonic, passphrase)`** (`:140-149`):
  - `salt = ("mnemonic" ‖ passphrase)[..min(256, len)]`. The cap includes the 8-byte prefix, so only the first 248 passphrase bytes count.
  - PBKDF2-HMAC-SHA512 over the raw mnemonic bytes, 2048 rounds, 64-byte output.
  - No Unicode normalisation of either input (the comment at `:147` assumes `[a-z ]`).
  - The passphrase is used as raw bytes, embedded NULs included (`test/functional/wallet_upgradetohd.py:258-267` shows `"custom-passphrase\0"` deriving a different wallet).
  - Every input and output is a `SecureString`/`SecureVector`.
- **Where `Check` is enforced.** Core rejects a phrase that fails `Check` before deriving anything:
  - legacy path: `CHDChain::SetMnemonic` (`wallet/hdchain.cpp:71-73`);
  - descriptor path: `CWallet::SetupDescriptorScriptPubKeyMans(mnemonic, passphrase)` (`wallet/wallet.cpp:4412-4414`).
  - Both throw `std::runtime_error`, which the RPC server turns into `RPC_MISC_ERROR` (−1). possible security note: see manager.
- **Passphrase length.**
  - The legacy path throws "Mnemonic passphrase is too long, must be at most 256 characters" above 256 bytes (`hdchain.cpp:60-62`).
  - The descriptor path has no check, and `ToSeed`'s truncation applies silently.
- **Storage.**
  - Legacy `CHDChain` keeps `vchSeed`, `vchMnemonic`, `vchMnemonicPassphrase` (`wallet/hdchain.h:35-37`). The chain ID is `Hash(vchSeed)` (`hdchain.cpp:128-132`).
  - Descriptor wallets store the mnemonic and passphrase next to the master key in `walletdescriptorkey` / `walletdescriptorckey` (`wallet/scriptpubkeyman.cpp:2510-2564`).
  - Both are encrypted with the wallet master key when the wallet is encrypted: `EncryptHDChain` `scriptpubkeyman.cpp:519-560`, `DescriptorScriptPubKeyMan::Encrypt` `:2269-2298`.
- **Paths.** BIP44 `m/44'/coin'/0'/{0,1}/i` (coin 5 on mainnet, 1 elsewhere), plus the inactive DIP9 CoinJoin descriptor `m/9'/coin'/4'/0'/0/*` for descriptor wallets (`scriptpubkeyman.cpp:2510-2564`).
- **Options.**
  - `-mnemonic`, `-mnemonicpassphrase`, `-mnemonicbits` (128/160/192/224/256, else `InitError`), `-usehd`, `-hdseed` (`wallet/init.cpp:104-108, 165-177`).
  - They apply to the first wallet created in the process and are then force-removed (`wallet.cpp:3254-3330`).
  - `createwallet` takes no mnemonic. A mnemonic arrives through `-mnemonic` or through `upgradetohd mnemonic mnemonicpassphrase walletpassphrase rescan` (`wallet/rpc/wallet.cpp:318-467`).
  - A born-encrypted wallet ignores `-mnemonic` and generates a random one (`wallet.cpp:438-481`).
  - `upgradetohd` on an encrypted wallet unlocks with `walletpassphrase`, sets up the seed, then relocks (`wallet/rpc/wallet.cpp:385-432`). possible security note: see manager.

#### 1.2.2 Imports (`wallet/rpc/backup.cpp`)

**Wallet-kind gates**
- Legacy-only RPCs call `EnsureLegacyScriptPubKeyMan` (`wallet/rpc/util.cpp:113-123`), which raises −4 "Only legacy wallets are supported by this command" on descriptor wallets.
- This covers `importprivkey` (`:144`), `importaddress` (`:258`), `importpubkey` (`:446`), `importmulti` (`:1560`), `importwallet` (`:515`), `dumphdinfo` (`:900`) and `dumpprivkey`. `test/functional/wallet_descriptor.py:84-92` asserts it.
- `importdescriptors` / `listdescriptors` refuse legacy wallets with −4 (`:1875`, `:2007`).

**`importprivkey privkey label rescan=true`** (`:107-197`)
1. A `DISABLE_PRIVATE_KEYS` wallet → −4 "Cannot import private keys to a wallet with private keys disabled" (`:140-142`).
2. The legacy gate.
3. Unlock required (−13).
4. Pruned rescan → −4, and a concurrent rescan → −4.
5. A bad WIF → −5 "Invalid private key encoding".
6. The key is imported with timestamp 1, so a rescan covers the whole chain.
7. The WIF's compression flag is kept.

**`importaddress address|script label rescan p2sh`** (`:225-321`)
- It works on watch-only wallets and imports a watch-only script with no solving data (`scriptpubkeyman.cpp:1794-1805`). A hex script is imported as a redeem script.
- `p2sh=true` with an address → −5.

**`importpubkey pubkey label rescan`** (`:417-490`)
- 33 or 65 bytes, fully valid.
- It imports the P2PKH script as watch-only with solving data.

**`importmulti`** (`:1472-1701`)
- Per-request results, timestamps and warnings.
- The `watchonly` flag only changes warnings. `IsMine` follows key availability.

**`importdescriptors`** (`:1811-1966`)
- Descriptor wallets only. It rescans from the lowest timestamp and requires an unlocked wallet.
- "Cannot import descriptor without private keys to a wallet with private keys enabled" (`:1766`): watch-only descriptors go only into `disable_private_keys` wallets.
- Dash has no segwit, so the descriptors are `pk`, `pkh`, `combo`, `multi`, `sortedmulti`, `sh`, `addr`, `raw`.

**Ownership levels.** `ISMINE_WATCH_ONLY = 1<<0`, `ISMINE_SPENDABLE = 1<<1` (`wallet/ismine.h:42-46`). Balances and listings report watch-only separately (`include_watchonly`, `test/functional/wallet_watchonly.py`).

#### 1.2.3 Watch-only and blank wallets

- **Flags.** `WALLET_FLAG_DISABLE_PRIVATE_KEYS = 1<<32` (`wallet/walletutil.h:46`), `WALLET_FLAG_BLANK_WALLET = 1<<33` (`:58`), `WALLET_FLAG_DESCRIPTORS = 1<<34` (`:61`), `WALLET_FLAG_EXTERNAL_SIGNER = 1<<35` (`:64`).
- **`createwallet`** (`wallet/rpc/wallet.cpp:602-707`) takes `wallet_name, disable_private_keys=false, blank=false, passphrase, avoid_reuse=false, descriptors=true, load_on_startup, external_signer=false`. Passing `descriptors` without `load_on_startup` → −8 (`:664-669`).
- **Blank help text** (`:610`): "A blank wallet has no keys or HD seed. One can be set using upgradetohd (by mnemonic) or sethdseed (WIF private key)."
- **Passphrase with private keys disabled** → −4 (`wallet.cpp:403-408`).
- **First run.** HD is initialised only if neither `DISABLE_PRIVATE_KEYS` nor `BLANK_WALLET` is set (`wallet.cpp:3254-3256`). `BLANK_WALLET` is cleared once a seed or descriptors are set (`scriptpubkeyman.cpp:2562`).
- **Watch-only wallets** refuse sends (−4 "Error: Private keys are disabled for this wallet") and `getnewaddress` (−4 "Error: This wallet has no available keys").

#### 1.2.4 Message signing (`util/message.cpp`, `key.cpp`, `pubkey.cpp`, `util/strencodings.cpp`)

- **`MESSAGE_MAGIC = "DarkCoin Signed Message:\n"`** (`util/message.cpp:25`).
- **`MessageHash(m)`** = `SHA256d(compactsize(25) ‖ magic ‖ compactsize(|m|) ‖ m)`, through `HashWriter` (`:74-80`). The message is an arbitrary byte string.
- **`MessageSign`** (`:58-72`) calls `CKey::SignCompact` (`key.cpp:254-275`): RFC 6979 nonce with no extra entropy (`:260`), header `27 + recid + (compressed ? 4 : 0)` (`:265`), 65 bytes, base64.
- **`MessageVerify(address, signature, message)`** (`util/message.cpp:27-56`) checks in this order:
  1. `ERR_INVALID_ADDRESS`: `DecodeDestination` fails for the current chain.
  2. `ERR_ADDRESS_NO_KEY`: not a `PKHash`.
  3. `ERR_MALFORMED_SIGNATURE`: `DecodeBase64` fails.
  4. `ERR_PUBKEY_NOT_RECOVERED`: `RecoverCompact` fails.
  5. `ERR_NOT_SIGNED`: `PKHash(recovered) ≠ destination`.
  6. `OK`.

  The enum is at `util/message.h:23-47`.
- **`RecoverCompact`** (`pubkey.cpp:199-217`): the size must be 65. `recid = (h − 27) & 3` and `compressed = ((h − 27) & 4) ≠ 0` are computed in `int`, so every header byte 0–255 maps to some recid and flag. Only the 65-byte length is enforced.
- **`DecodeBase64`** (`util/strencodings.cpp:139-172`):
  - the length must be a multiple of 4 (`:157`);
  - at most two trailing `=` are stripped (`:159-160`);
  - every remaining character must be in the standard alphabet;
  - leftover padding bits must be zero (`ConvertBits<6,8,false>`, `:164`);
  - `""` decodes to an empty vector.
- **RPCs.**
  - `verifymessage` (`rpc/signmessage.cpp:18-63`): −5 "Invalid address" (`:48`), −3 "Address does not refer to key" (`:50`), −3 "Malformed base64 encoding" (`:52`). Not recovered or not signed returns `false`.
  - `signmessagewithprivkey` (`:65-103`): −5 "Invalid private key" (`:91`), −5 "Sign failed" (`:97`).
  - `signmessage` (`wallet/rpc/signmessage.cpp:14-69`): unlock required (`:43`), −5 "Invalid address" (`:50`), −3 "Address does not refer to key" (`:55`), −5 "Sign failed" (`:61`), −4 "Private key not available" (`:63`).

#### 1.2.5 Encryption (`wallet/crypter.*`, `wallet/wallet.cpp`, `wallet/rpc/encrypt.cpp`)

- **Crypter.**
  - `CMasterKey`: the encrypted master key, an 8-byte salt, `nDerivationMethod = 0`, `nDeriveIterations = 25000` (`crypter.h:13-15, 33-59`).
  - `BytesToKeySHA512AES` is SHA512 iterated `count` times over `passphrase ‖ salt`. Key = digest bytes 0..32, IV = bytes 32..48 (`crypter.cpp:14-38`).
  - Secrets are AES-256-CBC under the master key, with IV = first 16 bytes of the pubkey hash, or of the chain ID for the HD chain.
- **`EncryptWallet`** (`wallet.cpp:792-908`):
  - It generates a random 32-byte master key and an 8-byte salt.
  - It calibrates iterations to about 100 ms with a floor of 25,000 (`:808-818`).
  - It writes `mkey` and encrypts every key store, including the HD chain and the mnemonic and passphrase (`:845-856`).
  - Descriptor wallets keep their seed (`:876-877`). Legacy non-HD wallets get a new keypool (`:889`).
  - It then locks and rewrites the DB (`:895-906`). No restart is needed.
- **Locking.**
  - `IsLocked(fForMixing)`: when `fOnlyMixingAllowed` is set, the wallet is "locked" for everything except callers that pass `true` (`:4158-4166`).
  - `Lock(fAllowMixing)` cleanses the master key unless mixing is allowed (`:4168-4186`).
  - `Unlock(passphrase, mixingOnly)` returns `true` without checking the passphrase if the wallet is already fully unlocked (`:4190-4191`). From Locked or mixing-only it tries each master key and runs `CheckDecryptionKey` on every key store (`:4188-4229`). `Lock(fAllowMixing=true)` keeps the master key and only sets the mixing-only flag (`:4168-4186`).
  - Under a mixing-only unlock the key store still serves keys to callers that ask `IsLocked(true)`: CoinJoin (`coinjoin/client.cpp:985, 1224, 1818`) and key-pool top-up (`scriptpubkeyman.cpp:1466, 1604`). User operations call `EnsureWalletIsUnlocked`, which uses `IsLocked()`, and stay refused.
  - `ChangeWalletPassphrase` (`:576-617`) records `fWasLocked = IsLocked(true)`, locks, unlocks Full with the old passphrase (a wrong one leaves the wallet Locked), re-wraps the master key with fresh calibration, and relocks only if it was fully locked before: mixing-only ends Full, Full stays Full.
- **RPCs** (`wallet/rpc/encrypt.cpp`; codes from `rpc/protocol.h:75-79`). An empty passphrase is −8 throughout.
  - **`walletpassphrase passphrase timeout mixingonly=false`** (`:10-124`):
    - −15 if the wallet is unencrypted (`:48`);
    - −8 for a negative timeout (`:60`);
    - a timeout above `100000000` is clamped (`:63-66`);
    - −17 for `mixingonly` while fully unlocked (`:76`);
    - −14 for a wrong passphrase (`:86-88`).
  - **`walletpassphrasechange`** (`:126-182`): −15, −4 (rescan in progress), −14.
  - **`walletlock`** (`:184-223`): −15, −4.
  - **`encryptwallet`** (`:225-285`): −16 if the wallet has no private keys (`:254`), −15 if it is already encrypted (`:258`), −16 on failure (`:276`).

---

## 2. API

Conventions: new modules follow existing crate style (`#[cfg_attr(feature = "serde", …)]`, `#[cfg_attr(feature = "bincode", derive(Encode, Decode))]`, `Zeroize` + `Drop` on secret holders, `edition = 2021` in these crates). New error types use `thiserror` 2 and are `#[non_exhaustive]` (D17).

### 2.1 Module layout

```
key-wallet/src/
  secret.rs                 SecretBytes, Passphrase (always on)
  mnemonic.rs               + MnemonicCompat, WalletMnemonic (additive)
  mnemonic/dash_core.rs     DashCoreMnemonic: Check/FromData/ToSeed ports, pbkdf2 one-block (always on)
  message.rs                message_hash, sign_message, verify_message, decode_base64_core (always on)
  imported.rs               ImportedKeys, ImportedEntry, ImportedKind, ImportBirth (always on; mutators pub(crate), D41)
  crypto/
    error.rs                CryptoError, TamperKind, LockState, SecretUse, UnlockMode (always on, so non-feature APIs can name them)
    params.rs               KdfParams, KdfBudget and their constants: plain data types, no argon2 (always on, D34)
    records.rs              MasterKeyRecord, SealedRecord, EncryptedWallet, SecretKind: data types and byte codecs, no crypto (always on, D51)
    mod.rs                  KdfPolicy, Crypter (argon2 + AEAD) (feature "encryption")
  wallet/
    initialization.rs       + from_mnemonic_with_passphrase, new_seedless (additive)
    lock.rs                 Wallet::{lock_state, require, account_origin}, AccountOrigin (always on, D52, D60)
    encryption.rs           Wallet::{encrypt, unlock, lock, lock_to_mixing_only, change_passphrase, …} (feature "encryption")
    file.rs                 versioned envelope v1 (§2.8)
key-wallet-manager/src/
  secrets.rs                WalletManager passphrase/import/lock/sign wrappers
```

`mnemonic.rs` becomes `mnemonic/mod.rs` only if the implementer prefers that; the public path `key_wallet::mnemonic::*` is unchanged.

### 2.2 Secret types (`secret.rs`)

```rust
/// Bytes that must never be logged, displayed or left in memory.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct SecretBytes(Vec<u8>);
impl PartialEq for SecretBytes { /* subtle::ConstantTimeEq, never derived (§5.2 item 3) */ }
impl Eq for SecretBytes {}
impl SecretBytes {
    pub fn new(bytes: Vec<u8>) -> Self;
    pub fn from_slice(bytes: &[u8]) -> Self;
    pub fn expose(&self) -> &[u8];          // the only accessor; grep-able
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
}
impl fmt::Debug for SecretBytes { /* "SecretBytes(<n> bytes)" */ }
// No Display, no Deref, no AsRef, no From<SecretBytes> for Vec<u8>.
// bincode Encode/Decode/BorrowDecode (feature "bincode"): length-prefixed bytes, decode into a zeroizing buffer;
// BorrowDecode so it can sit inside a derived type later (a derived Decode needs it on every field).
// serde: not implemented (D18).

/// A BIP39 mnemonic passphrase ("25th word"): raw bytes, as Core stores it (D4).
pub type Passphrase = SecretBytes;
```

### 2.3 Mnemonic compatibility (`mnemonic.rs`, `mnemonic/dash_core.rs`)

```rust
/// How a phrase is validated and turned into a seed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "bincode", derive(Encode, Decode))]
pub enum MnemonicCompat {
    /// BIP39 as specified: full checksum, NFKD on phrase and passphrase, all 10 word lists.
    #[default]
    Bip39,
    /// Dash Core's `CMnemonic` (bip39.cpp:78-149): English only, Core's checksum mask,
    /// raw bytes (no NFKD), salt ("mnemonic" ‖ passphrase) cut at 256 bytes.
    DashCore,
}

/// A phrase accepted by `CMnemonic::Check`. Holds the exact bytes Core would hash.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct DashCoreMnemonic { phrase: SecretBytes }
// Manual, so `WalletMnemonic`'s derives and `WalletType`'s derived `Debug` compile (D37):
impl fmt::Debug for DashCoreMnemonic { /* "DashCoreMnemonic(<n> words)": never a word or byte */ }
#[cfg(feature = "bincode")] impl bincode::Encode for DashCoreMnemonic { /* the phrase bytes, as SecretBytes's codec */ }
#[cfg(feature = "bincode")] impl<C> bincode::Decode<C> for DashCoreMnemonic {
    /* decode into a zeroizing buffer, then `DashCoreMnemonic::parse`: a phrase Core's Check
       rejects is DecodeError::OtherString("invalid Dash Core mnemonic"), never the phrase.
       Same shape as the existing `Mnemonic` codec (mnemonic.rs:123-146), plus BorrowDecode. */
}

impl DashCoreMnemonic {
    /// Verbatim port of `CMnemonic::Check` (bip39.cpp:78-138), byte for byte, including the mask.
    pub fn parse(phrase: &[u8]) -> Result<Self, MnemonicError>;
    /// `CMnemonic::Generate` (bip39.cpp:33-42). `bits` ∈ {128,160,192,224,256} else Err (Core's -mnemonicbits rule).
    #[cfg(feature = "getrandom")] pub fn generate(bits: u32) -> Result<Self, MnemonicError>;
    /// `CMnemonic::FromData` (bip39.cpp:45-76).
    pub fn from_entropy(entropy: &[u8]) -> Result<Self, MnemonicError>;
    /// `CMnemonic::ToSeed` (bip39.cpp:140-149): no normalisation, salt cut at 256 bytes.
    pub fn to_seed(&self, passphrase: &[u8]) -> Zeroizing<[u8; 64]>;
    pub fn phrase(&self) -> &SecretBytes;
    pub fn word_count(&self) -> usize;
    /// True iff the phrase also passes strict BIP39 (full checksum).
    pub fn is_strict_bip39(&self) -> bool;
}

/// Free functions for callers that only need the predicate or the seed.
pub fn core_check(phrase: &[u8]) -> bool;                                   // == DashCoreMnemonic::parse(..).is_ok()
pub fn core_seed(phrase: &[u8], passphrase: &[u8]) -> Zeroizing<[u8; 64]>;  // ToSeed with no Check, as Core's ToSeed
pub const CORE_SALT_MAX_BYTES: usize = 256;
pub const CORE_LEGACY_MAX_PASSPHRASE_BYTES: usize = 256;                   // hdchain.cpp:60-62, enforced by the caller (D5)

/// The mnemonic a wallet keeps, in the form that reproduces its seed.
#[derive(Clone, Debug, Zeroize, ZeroizeOnDrop)]          // Debug: Mnemonic's and DashCoreMnemonic's redacted impls (D40)
#[cfg_attr(feature = "bincode", derive(Encode, Decode))]
pub enum WalletMnemonic {
    Bip39(Mnemonic),
    DashCore(DashCoreMnemonic),
}
impl WalletMnemonic {
    pub fn parse(phrase: &str, compat: MnemonicCompat) -> Result<Self, MnemonicError>;
    pub fn compat(&self) -> MnemonicCompat;
    /// Bip39: requires UTF-8 passphrase (NFKD); DashCore: raw bytes.
    pub fn to_seed(&self, passphrase: &Passphrase) -> Result<Zeroizing<[u8; 64]>, MnemonicError>;
    /// Explains where Bip39 and DashCore would disagree for this phrase+passphrase (UI warnings, WL3 dual derivation).
    pub fn portability(phrase: &[u8], passphrase: &[u8]) -> Portability;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Portability {
    pub core_accepts: bool,
    pub bip39_accepts: bool,          // English, full checksum
    pub nfkd_changes_passphrase: bool,
    pub salt_truncated: bool,         // 8 + |passphrase| > 256
}
impl Portability { pub fn seeds_agree(&self) -> bool; }

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum MnemonicError {
    #[error("mnemonic is empty")] Empty,
    #[error("mnemonic word count {0} is not 12, 15, 18, 21 or 24")] WordCount(usize),
    #[error("mnemonic word {index} is not in the word list")] UnknownWord { index: usize },  // never the word itself
    #[error("mnemonic checksum mismatch")] Checksum,
    #[error("entropy length {0} is not 16..=32 bytes in steps of 4")] EntropyLength(usize),
    #[error("mnemonic bits {0} not in 128, 160, 192, 224, 256")] Bits(u32),
    #[error("passphrase is not valid UTF-8 (required for BIP39 NFKD)")] PassphraseNotUtf8,
    #[error("passphrase is {0} bytes; at most 256 allowed")] PassphraseTooLong(usize),
}
```

**Existing `Debug` impls redacted as hardening (WL1-2, D40).** Two existing types get a manual `Debug`:
- `Mnemonic` → `Mnemonic { language: English, words: 12 }`. Today it already prints only the language (bip39 2.2.2), so this is not a leak fix; it pins the format against a future bip39 bump.
- `RootExtendedPrivKey` → `RootExtendedPrivKey { .. }`, the same standard as the new types.

No consumer reads either format (searched across key-wallet, key-wallet-manager, key-wallet-ffi, dash-spv and dash-spv-ffi); uses that only need the `Debug` bound keep compiling. **Test (WL1-2):** for every secret-bearing type reachable from `Wallet` (`Mnemonic`, `DashCoreMnemonic`, `WalletMnemonic`, `SecretBytes`, `Seed`, `RootExtendedPrivKey`, `ExtendedPrivKey`, `WalletType` variants 0, 1, 2 and 5, `Wallet`; later slices add the imported types and `EncryptedWallet`), `{:?}` and `{:#?}` contain no secret byte sequence: no phrase word, no seed, private key or **chain code** byte in hex, decimal-array or ASCII form, and no WIF/xprv string. The chain code counts as secret for this test.

Port notes:
- **`parse`** keeps Core's exact tokenisation: count `' '` + 1, no trimming, the 10-character guard and a case-sensitive linear lookup. The word list is the `bip39` crate's English list, which is byte-equal to `bip39_english.h`. A unit test asserts that by checksum against a transcribed copy.
- **`MnemonicError` never carries the phrase or any word**, only positions and counts (§5).
- **PBKDF2** is one block of 64 bytes over `dashcore_hashes::{Hmac, sha512}`, the same construction as dw-compat's `pbkdf2_hmac_sha512_one_block`, so there is no new dependency. A test checks it against the `bip39` crate for random ASCII inputs below the truncation point.

### 2.4 Passphrase end to end (wallet types and constructors)

**`WalletType`.** Variants are appended. Indices 0–4 are unchanged. From WL1-1 on, `WalletType`'s bincode codec is hand-written with **explicit tags 0–7** (D26), so neither declaration order, a `cfg`-gated variant, nor the order in which slices merge can shift a tag.

```rust
pub enum WalletType {
    Mnemonic { mnemonic: Mnemonic, root_extended_private_key: RootExtendedPrivKey },          // [0] unchanged
    Seed { seed: Seed, root_extended_private_key: RootExtendedPrivKey },                       // [1] unchanged
    ExtendedPrivKey(RootExtendedPrivKey),                                                      // [2] unchanged
    ExternalSignable,                                                                          // [3] unchanged
    WatchOnly,                                                                                 // [4] unchanged
    /// [5] A mnemonic with a non-empty passphrase, or any DashCore-mode mnemonic.
    MnemonicWithPassphrase {
        mnemonic: WalletMnemonic,
        passphrase: Passphrase,
        root_extended_private_key: RootExtendedPrivKey,
    },
    /// [6] Private keys allowed but no HD seed: Core `blank=true` or legacy non-HD.
    /// Holds only imported keys (`Wallet::imported`). ID is random and stored (D10).
    Seedless,
    /// [7] Every secret sealed (§2.6). Public metadata only, plus a transient unlocked copy.
    /// Always compiled (D51); only unlocking needs feature "encryption".
    Encrypted(Box<EncryptedWallet>),
}

impl Drop for WalletType { fn drop(&mut self) { self.zeroize() } }   // new in WL1-1 (D45)
```

Notes:
- **`Encrypted` without the feature (D51).** The variant and its data types (`crypto/records.rs`) are always compiled, so `WalletType` has the same variants in every build and enabling `encryption` anywhere in a dependency graph cannot break a downstream exhaustive match. Without the feature an encrypted envelope decodes, re-encodes byte-equal and stays `Locked`; public derivation works; there is no `unlock`/`encrypt` to call. This replaces rev3's "decoding fails without the feature" (D16 now covers only the crypto dependencies).
- **Legacy decoder and tags 5–7 (n4).** The legacy (non-envelope) `Wallet`/`WalletType` decoder rejects tags 5, 6 and 7 in every build, with or without any feature. They appear only inside envelope v1 (§2.8), whose writer is the only producer.
- **`Drop for WalletType` (D45).** `WalletType` derives `Zeroize` but has no `Drop` today, so a dropped `Seed` variant (`Seed` is `Copy` and cannot wipe itself) leaves its 64 bytes in freed memory. WL1-1 adds `Drop` calling the derived `zeroize()`, which wipes every secret-bearing arm (0, 1, 2, 5) on drop. No code in key-wallet, key-wallet-manager or key-wallet-ffi moves a field out of an owned `WalletType` (searched), so the impl breaks no pattern.
- **serde (D18).** `WalletType` and `Wallet` keep their serde derives for the existing data.
  - Variants 5–7 carry `#[cfg_attr(feature = "serde", serde(skip))]`: serialising a wallet that uses them is an error, never a silent loss, and deserialising can never produce them. The skipped variants' fields need no serde impls (the implementer confirms with `cargo check -p key-wallet --features serde`).
  - `Wallet::imported` (§2.5) is declared as
    ```rust
    #[cfg_attr(feature = "serde", serde(
        default,                                      // absent field → ImportedKeys::default() (empty)
        skip_deserializing,                           // input never populates it; ImportedKeys has no Deserialize
        skip_serializing_if = "ImportedKeys::is_empty",
        serialize_with = "imported::refuse_serde",    // reached only when non-empty → returns S::Error::custom(...)
    ))]
    imported: ImportedKeys,                           // private (D41); read through Wallet::imported()
    ```
    An empty set is omitted, so a pre-WL1-shaped wallet serialises to exactly the old output in any serde format. A non-empty set refuses to serialise. Because no serde output ever carries the field, `skip_deserializing` loses nothing; an input that injects an `imported` key is ignored (`Wallet` has no `deny_unknown_fields`) and yields an empty set. `ImportedKeys` implements neither `Serialize` nor `Deserialize`.
  - **Tests (WL1-1 snapshots, WL1-5 field; D79).** serde_json cannot encode a wallet that has DashPay or platform-payment accounts: those maps have struct keys, and every default wallet has a `PlatformPayment` account. The five-variant serde pins are therefore bincode-serde snapshots. WL1-1 committed `<variant>.serde.bin` (`bincode::serde`, `standard()`) next to each layout golden, produced by the unchanged serde derives (W1-3). They run in **key-wallet-ffi**, whose dev-dependency enables bincode's `serde` feature; key-wallet's own tests cannot decode them. A positional format cannot express an injected key or exercise the human-readable path, so one **serde_json snapshot** remains: `account_free.json`, a variant-0 wallet built with BIP44, BIP32, CoinJoin and identity accounts only (no DashPay or platform-payment account). WL1-2's first commit generates it on the WL1-1 head (D89). WL1-2 and WL1-4 add only `serde(skip)` variants, which leave the output of existing variants unchanged, and the `.serde.bin` goldens prove that for all five. After WL1-5, in key-wallet-ffi:
    - each `.serde.bin` snapshot deserialises to a wallet equal (§2.8 equality helper) to the fixture wallet and re-serialises byte-equal;
    - `account_free.json` does the same through serde_json and still serialises to exactly the old JSON;
    - a wallet with a non-empty `imported`, and each of variants 5–7, fails to serialise through both formats;
    - `account_free.json` with an injected `"imported"` key deserialises with an empty set.
    `ManagedWalletInfo`'s serde form does not change at all (§2.5, D83): its imported accounts are `serde(skip)`. One serde_json snapshot pins it, `managed_account_free.json`, the managed info of the `account_free.json` wallet. After WL1-5 it deserialises and re-serialises byte-equal, and a managed info holding imported accounts serialises byte-equal to the same info without them.
    **Where the two JSON snapshots come from (D89).** WL1-2's first commit generates `account_free.json` and `managed_account_free.json` on the WL1-1 head, before WL1-2 changes any serde path (WL1-1 changed only the bincode codec). So "exactly the old JSON" means pre-WL1 output, including the human-readable paths that the `.serde.bin` goldens do not cover.
- **`id_origin` (D42).** `Wallet` gains an in-memory field, outside the legacy layout:
    ```rust
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub enum IdOrigin { #[default] Derived, Stored }

    #[cfg_attr(feature = "serde", serde(default, skip_deserializing,
        skip_serializing_if = "IdOrigin::is_derived", serialize_with = "imported::refuse_serde"))]
    id_origin: IdOrigin,                               // read through Wallet::id_origin()
    ```
    - It is `Stored` for every wallet made by `new_seedless*`, kept by `upgrade_to_hd*`, `encrypt`, `unlock`, `lock` and `downgrade_to_external_signable`, and read from (and written to) envelope flag bit 0. A legacy decode, and every pre-WL1 constructor, gives `Derived`. A `Stored` wallet is a WL1 feature: the legacy encoders refuse it, and serde refuses to serialise it (same pattern as `imported`).
    - `compute_wallet_id()` returns `self.wallet_id` whenever `id_origin == Stored`, for every variant. For `Derived` it behaves as today, plus the explicit arms below. `unlock` never changes `wallet_type` (the plaintext lives in `EncryptedWallet`'s transient state), so an unlocked encrypted wallet's ID is the stored one too.
    - **`WalletManager` call sites.** The map key is `wallet.compute_wallet_id()` at seven places: `accessors.rs:64` (`insert_wallet`), `lib.rs:287, 372, 411, 452, 500, 548` (the create, add and import paths). WL1-5 keeps each call but relies on the origin-aware result, and adds one test per site: a stored-ID wallet (blank → `upgrade_to_hd`, and blank → `encrypt` → `upgrade_to_hd_encrypted`) inserted, created or imported through that path is keyed by `wallet.wallet_id`, and `get_wallet(&wallet.wallet_id)` finds it. A **re-register** test drops the manager, rebuilds it from `to_bytes()` output through `import_wallet_from_bytes` and through `insert_wallet(Wallet::from_bytes(..))`, and finds the wallet under the same ID. Legacy (`Derived`) imports keep today's key.
- **Exhaustive matches.** Every match on `WalletType` in key-wallet gains the new arms: `helper.rs:80-123, 376-394`, `initialization.rs:136-155`, `wallet/mod.rs:153-161` (`compute_wallet_id`) and `root_extended_keys.rs:361-423` (the root-key accessors). In particular:
  - **Wallet ID (D36, D42).** After the `Stored` check above, `compute_wallet_id()` gets explicit arms `WalletType::Seedless | WalletType::Encrypted(_) => self.wallet_id`, beside the existing `WatchOnly | ExternalSignable` arm. Neither variant ever reaches the `root_extended_pub_key_cow().expect(…)` fallback: a seedless wallet has no root key and its ID is random and stored (D10); an encrypted wallet's ID was fixed before encryption and is stored in clear. The `_ =>` fallback is replaced by the explicit list of root-key variants, so a future variant fails to compile instead of panicking.
  - **Root-key accessors (D36).** `root_extended_pub_key`, `root_extended_pub_key_cow` and `root_extended_priv_key` never `expect` and return existing `key_wallet::Error` variants (no new variant, D17):
    - `Seedless` → `Error::InvalidParameter("seedless wallet has no root key; use wallet.wallet_id for identity")` from all three.
    - `Encrypted` → the two public accessors return the public `root_xpub` (a `RootExtendedPubKey`, n2; D20) when the sealed kind is HD, and the seedless error above when it is `Seedless`. `root_extended_priv_key` calls `require(SecretUse::General)`: locked or mixing-only gives `Error::InvalidState("wallet is locked")`, unlocked borrows from the transient plaintext.
  - **ID fixtures (WL1-5, WL1-6):** `create_seedless_wallet` returns an ID equal to `wallet.wallet_id` and to `compute_wallet_id()`, stable across two calls; `import_wallet_from_bytes` of a seedless envelope and of an encrypted envelope (locked) returns the stored ID and does not panic; `encrypt` leaves `wallet_id` and `compute_wallet_id()` unchanged for every variant it accepts (0, 1, 2, 5, 6); the root-key accessors on `Seedless` and on a locked `Encrypted` wallet return the errors above.
  - `has_mnemonic()` is true for `MnemonicWithPassphrase`, and true for `Encrypted` when its `kind` says so.
  - **`has_seed()` (`helper.rs:100-102`, D38)** is true for `Mnemonic`, `Seed` and `MnemonicWithPassphrase`, and for `Encrypted` iff its `SecretKind` is `Mnemonic`, `MnemonicWithPassphrase` or `Seed`. It answers "does this wallet have a seed", not "is it unlocked". `create_accounts_from_options` (`helper.rs:340-359`) uses it to add the BLS operator and EdDSA platform accounts, so leaving variant 5 out would silently drop them.
    - **Default special-purpose accounts never need an unlock.** `BLSAccount`/`EdDSAAccount` hold only public keys (`account/bls_account.rs:29-40`). They are created from the seed while it is in plaintext: at construction (before `encrypt` seals anything) or, in `upgrade_to_hd_encrypted`, step 3 builds the new accounts on a plaintext wallet off to the side from the mnemonic just supplied, before the new kind-1 record is sealed. After sealing, the accounts live in the public `AccountCollection`.
    - A later explicit `add_bls_account`/`add_eddsa_account` without a supplied seed (`accounts.rs:93-120, 158-185`) migrates to `seed_zeroizing` (§2.4.1) and on a locked or mixing-only encrypted wallet returns `Error::InvalidState("wallet is locked")`.
  - **Predicate sweep (D38).** The `Wallet` predicates on `WalletType` are `has_mnemonic`, `is_watch_only`, `is_external_signable`, `can_sign` and `has_seed` (`helper.rs:80-102`); there are no others in key-wallet. `is_watch_only` and `is_external_signable` name one existing variant each and stay false for 5–7, which is correct (a seedless wallet has private keys enabled, and `encrypt` refuses both). `can_sign` (`!matches!(WatchOnly)`) is already true for 5–7, as specified below. `has_mnemonic`, `has_seed` and `can_sign` become exhaustive `match`es so a future variant fails to compile instead of falling into a wildcard.
  - `can_sign()` is true for `Seedless` (it can sign imported keys). For `Encrypted`, `can_sign()` reports whether the wallet holds keys at all, and signing while locked returns `Error::InvalidState("wallet is locked")` (D17).
  - `wallet_seed_bytes()` uses the stored passphrase and compat mode for variant 5 (§2.4.1).
  - **Test (WL1-2, WL1-6), with `--features bls,eddsa`:** default `WalletAccountCreationOptions` give the same set of `AccountType`s, including `ProviderOperatorKeys` and `ProviderPlatformKeys`, for variant 0, variant 5 (non-empty passphrase, and DashCore mode) and an encrypted wallet sealed from each (locked). For a strict phrase in DashCore mode with an empty passphrase, the variant-5 BLS and EdDSA public keys equal variant 0's (same seed). Encrypting leaves every account and key unchanged; `upgrade_to_hd_encrypted` produces the same account set as the unencrypted upgrade.

**`Wallet` constructors.** These are additive; the existing ones are unchanged.

```rust
impl Wallet {
    /// Passphrase and compat aware. For (Bip39, empty passphrase) the result is exactly
    /// `from_mnemonic` (WalletType::Mnemonic, same bytes and ID) (D2).
    pub fn from_mnemonic_with_passphrase(
        mnemonic: WalletMnemonic,
        passphrase: Passphrase,
        network: Network,
        options: WalletAccountCreationOptions,
    ) -> Result<Self>;
    /// Parses with `compat` and calls the above.
    pub fn from_phrase(phrase: &str, passphrase: Passphrase, compat: MnemonicCompat,
                       network: Network, options: WalletAccountCreationOptions) -> Result<Self>;
    /// Core blank wallet with private keys enabled.
    #[cfg(feature = "getrandom")] pub fn new_seedless(network: Network) -> Self;
    pub fn new_seedless_with_id(network: Network, wallet_id: [u8; 32]) -> Self;   // WL3 import keeps IDs
    /// Core `upgradetohd` on an unencrypted Seedless wallet: adds an HD seed, keeps `imported`
    /// and the stored ID (D10, D42). Error order as Core (D47): AlreadyHd → PrivateKeysDisabled →
    /// (Encrypted: Crypto(EmptyPassphrase), Core's "encrypted but supplied empty wallet passphrase";
    /// use upgrade_to_hd_encrypted, §2.6.1) → the phrase is parsed only now → Mnemonic(..).
    pub fn upgrade_to_hd(&mut self, phrase: &str, compat: MnemonicCompat, passphrase: Passphrase,
                         options: WalletAccountCreationOptions) -> Result<(), UpgradeError>;
    /// The passphrase, if any (for `dumphdinfo` / `listdescriptors true`). Requires unlocked.
    pub fn mnemonic_passphrase(&self) -> Result<Option<&Passphrase>>;
}
```

#### 2.4.1 Existing seed and phrase accessors (narrow change, D27)

Today `Mnemonic::to_seed` returns a bare `[u8; 64]` (`mnemonic.rs:267-271`), `Mnemonic::phrase` a plain `String` (`:247`), `Wallet::wallet_seed_bytes` a bare `Option<[u8; 64]>` (`wallet/helper.rs:110-123`), and `Seed` is `Copy` (`seed.rs:16`). WL1 keeps every one of these signatures and adds zeroizing siblings:

| Existing (unchanged signature) | WL1 adds | Deprecated? |
|---|---|---|
| `Mnemonic::to_seed(&self, &str) -> [u8; 64]` | `Mnemonic::to_seed_zeroizing(&self, &str) -> Zeroizing<[u8; 64]>` | yes, `#[deprecated(note = "use to_seed_zeroizing")]` |
| `Mnemonic::phrase(&self) -> String` | `Mnemonic::phrase_zeroizing(&self) -> Zeroizing<String>` | yes |
| `Wallet::wallet_seed_bytes(&self) -> Option<[u8; 64]>` | `Wallet::seed_zeroizing(&self) -> Result<Option<Zeroizing<[u8; 64]>>, CryptoError>` (`CryptoError` is always compiled; without feature `encryption` no wallet can be locked, so the error is never returned) | yes |
| `Seed` (`Copy`) | — | no; removing `Copy` would break every consumer. WL1 adds no path that creates a `Seed`; `WalletType::Seed` keeps it |

Rules:
- **Internal callers move.** Every non-test caller in key-wallet, key-wallet-manager and key-wallet-ffi moves to the zeroizing sibling in the slice that deprecates (WL1-2): `initialization.rs:108, 225`, `helper.rs:115`, the derivation helpers, `Mnemonic`'s bincode codec (`mnemonic.rs:123-146`, below), and `key-wallet-ffi/src/mnemonic.rs:209` (`mnemonic_to_seed`; its C signature is unchanged and it still copies into the caller's buffer). Test-only call sites either migrate or carry `#[allow(deprecated)]`. Clippy `-D warnings` enforces the migration in the workspace.
- **`Mnemonic`'s bincode codec (D45).** `encode` writes the phrase from `phrase_zeroizing()`; `decode`/`borrow_decode` read the `String` and move it into `Zeroizing<String>` before parsing (the allocation is the one the decoder produced, so no other copy remains). Every `encrypt`, `upgrade_to_hd_encrypted` and unlock of a Bip39 wallet goes through this codec, so it is in WL1's zeroization scope.
- **Variant 5** is answered by the deprecated `wallet_seed_bytes` too, with the stored passphrase and compat mode, so old callers get a correct seed, not the empty-passphrase one.
- **Locked extraction (WL1-6).** On `WalletType::Encrypted`, the deprecated `wallet_seed_bytes` returns `None` whether or not the wallet is unlocked (it has no error channel), and `phrase`/`to_seed` are unreachable because no `Mnemonic` is handed out. `seed_zeroizing` calls `require(SecretUse::Export)` and returns `Err(Locked)` or `Err(MixingOnly)` unless fully unlocked.
- **Scope of the zeroization claim (§5.2 item 5).** It covers WL1's new types, the zeroizing siblings and the code paths WL1 adds or migrates. It does not cover the deprecated accessors or values a consumer copies out of them, nor `Seed` copies. possible security note: see manager.

**`upgrade_to_hd` and the wallet ID.** Upgrading changes `wallet_type` from `Seedless` to `MnemonicWithPassphrase` or `Mnemonic`, but `id_origin` stays `Stored`, so `compute_wallet_id()` keeps returning the stored ID and every `WalletManager` path keys the wallet by it (D10, D42).

**`WalletManager`.** These additions mirror the existing creation API.

```rust
impl<T: WalletInfoInterface + …> WalletManager<T> {
    pub fn create_wallet_from_mnemonic_with_passphrase(
        &mut self, phrase: &str, passphrase: Passphrase, compat: MnemonicCompat,
        birth_height: CoreBlockHeight, options: WalletAccountCreationOptions,
    ) -> Result<WalletId, WalletError>;
    pub fn create_seedless_wallet(&mut self, birth_height: CoreBlockHeight) -> Result<WalletId, WalletError>;
}
```

### 2.5 Imported, watch-only and seedless accounts (`imported.rs`, managed side)

**Account kinds.** The new kinds are appended, so indices 0–14 are unchanged. They have no xpub, are never stored in `AccountCollection` (which is xpub-based), and have no derivation path.

```rust
pub enum AccountType {
    …15 existing variants unchanged…,
    /// Keys imported with their private key (Core ISMINE_SPENDABLE, importprivkey / WIF sweep / legacy keypool).
    ImportedKeys,
    /// Scripts, addresses and pubkeys imported without a private key (Core ISMINE_WATCH_ONLY).
    ImportedWatchOnly,
}
```

**Wallet-side container.** It holds public data, and secrets in one of two forms: plaintext in an unencrypted wallet, sealed in an encrypted one (D41).

```rust
#[derive(Clone, Default)]                              // no Zeroize derive: ScriptBuf, PublicKey and BTreeMap have no Zeroize impl (D35)
pub struct ImportedKeys {
    entries: BTreeMap<ScriptBuf, ImportedEntry>,       // keyed by the primary scriptPubKey (P2PKH for keys/pubkeys)
    aliases: BTreeMap<ScriptBuf, ScriptBuf>,           // P2PK(pubkey) → its P2PKH primary (D48)
}

#[derive(Clone)]
pub struct ImportedEntry {
    pub script_pubkey: ScriptBuf,
    pub kind: ImportedKind,
    pub label: Option<String>,
    pub birth: ImportBirth,
}

#[derive(Clone)]
pub enum ImportedKind {
    /// A private key in an unencrypted wallet. The pubkey's `compressed` flag is the WIF flag and
    /// fixes the P2PKH script (Core keeps it). Never present while the wallet is Encrypted.
    PrivateKey { pubkey: PublicKey, secret: Zeroizing<[u8; 32]> },
    /// A private key in an encrypted wallet: the kind-2 record (§2.6), opaque until unlocked.
    /// Never present while the wallet is unencrypted.
    SealedPrivateKey { pubkey: PublicKey, record: SealedRecord },
    /// A public key (importpubkey): watch-only with solving data.
    PublicKey(PublicKey),
    /// An address or raw script (importaddress): watch-only, no solving data; optional redeem script.
    Script { redeem_script: Option<ScriptBuf> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportBirth { Genesis, Height(CoreBlockHeight), Time(u64) }

impl ImportedKeys {                                    // read side public, mutators pub(crate) (D41)
    pub fn get(&self, script: &ScriptBuf) -> Option<&ImportedEntry>;   // resolves P2PK aliases
    pub fn entries(&self) -> impl Iterator<Item = &ImportedEntry>;
    pub fn spendable_scripts(&self) -> impl Iterator<Item = &ScriptBuf>;   // primaries and aliases of key entries
    pub fn watch_only_scripts(&self) -> impl Iterator<Item = &ScriptBuf>;
    pub fn earliest_birth(&self) -> Option<ImportBirth>;
    pub fn is_empty(&self) -> bool;
    pub(crate) fn insert_private(&mut self, ..) -> ImportOutcome;
    pub(crate) fn insert_sealed(&mut self, ..) -> ImportOutcome;
    pub(crate) fn insert_public(&mut self, ..) -> ImportOutcome;
    pub(crate) fn insert_script(&mut self, ..) -> ImportOutcome;
    pub(crate) fn seal_all(&mut self, ..);             // PrivateKey → SealedPrivateKey (encrypt), wipes plaintext
    pub(crate) fn drop_secrets(&mut self);             // downgrade_to_external_signable (D57)
    pub(crate) fn remove(&mut self, script: &ScriptBuf) -> bool;   // entry and its P2PK alias (D85)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportOutcome { Added, UpgradedToSpendable, AlreadyPresent }

// Manual, secret-only wiping (D35).
impl Zeroize for ImportedKind  { /* PrivateKey { secret, .. } => secret.zeroize(); other arms: nothing (a sealed record is ciphertext) */ }
impl Zeroize for ImportedEntry { /* self.kind.zeroize() */ }
impl Zeroize for ImportedKeys  { /* for e in entries.values_mut() { e.zeroize() } */ }
impl Drop for ImportedKeys     { fn drop(&mut self) { self.zeroize() } }

// Manual, redacted Debug, so `Wallet`'s derived Debug (wallet/mod.rs:72-75) never prints a secret.
impl fmt::Debug for ImportedKind { /* PrivateKey { pubkey, secret: <redacted> }, SealedPrivateKey { pubkey, .. }, PublicKey(..), Script { .. } */ }
impl fmt::Debug for ImportedEntry { /* script_pubkey, kind (redacted as above), label, birth */ }
impl fmt::Debug for ImportedKeys { /* "ImportedKeys { entries: <n>, private: <m>, .. }" plus the entries */ }
```

**The `Wallet` import API (D41).** `Wallet.imported` is private; `Wallet::imported() -> &ImportedKeys` is the only public access. Every write goes through `Wallet`, which applies the gates:
```rust
impl Wallet {
    pub fn imported(&self) -> &ImportedKeys;
    pub fn import_private_key(&mut self, key: &PrivateKey, label: Option<String>, birth: ImportBirth) -> Result<ImportOutcome, ImportError>;
    pub fn import_public_key(&mut self, key: PublicKey, label: Option<String>, birth: ImportBirth) -> Result<ImportOutcome, ImportError>;
    /// importaddress with an address (backup.cpp:287-295): watch its scriptPubKey, no solving data.
    pub fn import_address(&mut self, address: &Address, label: Option<String>, birth: ImportBirth) -> Result<ImportOutcome, ImportError>;
    /// importaddress with a hex script (backup.cpp:296-307): watch `script` itself and record it as a
    /// redeem script; with `p2sh`, also watch P2SH(script). Returns one outcome per watched script.
    pub fn import_raw_script(&mut self, script: ScriptBuf, p2sh: bool, label: Option<String>, birth: ImportBirth) -> Result<Vec<(ScriptBuf, ImportOutcome)>, ImportError>;
    /// The private key for an imported script (P2PKH or its P2PK alias). Unencrypted: copied from
    /// the entry. Encrypted: `require(use_)` first (D12: MixingOnly admits CoinJoin and KeyPoolTopUp,
    /// as for HD keys, D67), then the kind-2 record is decrypted on demand. Nothing is cached.
    pub fn imported_signing_key(&self, script: &ScriptBuf, use_: SecretUse) -> Result<Option<Zeroizing<[u8; 32]>>, CryptoError>;
    /// Forget the entry that holds `script` (a primary or a P2PK alias), wiping its secret. Watch-only and
    /// unencrypted entries: any state. A sealed entry needs `Full`, since the kind-1 records digest is
    /// resealed (D61); otherwise `ImportError::Locked`. The reseal uses D62's stage-then-commit: a failure
    /// (`Rng`) leaves `self` unchanged, and the secret is wiped only after the commit (D96).
    /// `Ok(false)` when no entry holds the script (D85).
    pub fn remove_imported(&mut self, script: &ScriptBuf) -> Result<bool, ImportError>;
}
```
- **Gates for `import_private_key`.** `WatchOnly` and `ExternalSignable` → `ImportError::PrivateKeysDisabled`. `Encrypted` and `Locked` → `ImportError::Locked(CryptoError::Locked)`; `Encrypted` and mixing-only → `ImportError::Locked(CryptoError::MixingOnly)` (Core's `EnsureWalletIsUnlocked` −13, `backup.cpp:144`). Without feature `encryption` an `Encrypted` wallet is always `Locked` (D51), so the first error applies. `Encrypted` and `Full` → the key is sealed at once into a new kind-2 record under the MK, the entry is stored as `SealedPrivateKey`, and the kind-1 record is resealed with the new records digest (§2.6, D43). Unencrypted → `PrivateKey`.
- **Atomic import while Full (D62).** Sealing draws fresh nonces, which can fail (`CryptoError::Rng`). The import therefore stages everything first: the new kind-2 record, the new entry (or the `UpgradedToSpendable` replacement) and the resealed kind-1 record are built off to the side, and only then committed together by moves that cannot fail. Any error returns with `self` byte- and state-identical to before, so a saved wallet never holds a kind-2 set its kind-1 record does not cover. Test (WL1-6): an RNG fault injected before the kind-2 seal and another before the kind-1 reseal (the §2.6.1 test hook) each return `Rng`, leave `to_bytes()` unchanged, and the wallet still unlocks and signs with its old keys.
- **Watch-only imports** (`import_public_key`, `import_address`, `import_raw_script`) are allowed in every lock state, as in Core (watch-only scripts need no key). They are not authenticated by the records digest (§5.1).
- **Lifecycle (D41).** `encrypt` turns every `PrivateKey` entry into `SealedPrivateKey` (one kind-2 record each) and wipes the plaintext before it returns. `lock` wipes only the MK and the kind-1 plaintext; sealed entries are ciphertext and stay. `unlock` decrypts no imported key except to run the check in §2.6. `to_bytes` writes one kind-2 record per `SealedPrivateKey` entry, so a save never drops a key, whatever the lock state. There is no plaintext copy of an imported key in an encrypted wallet at any time, except inside `imported_signing_key`'s returned `Zeroizing` buffer.
- **Downgrade (D57).** `downgrade_to_external_signable` keeps its signature. It turns every private entry (`PrivateKey` or `SealedPrivateKey`) into a `PublicKey(pubkey)` watch-only entry, wiping the plaintext secret or dropping the kind-2 record; on an `Encrypted` wallet it also drops the MK record, the kind-1 record and any unlocked state, wiping them first. `id_origin` and the watch-only entries are kept. **Managed side (D70):** `WalletManager::downgrade_to_external_signable(wallet_id)` (new wrapper, WL1-5) also moves the managed `ImportedKeys` pool's scripts, UTXOs and balance to `ImportedWatchOnly`; a caller of the bare `Wallet` method owns that resync, and the method's docs say so (Platform, the only caller today, cannot import keys in WL1, so it has nothing to move). Test: a wallet with two imported keys (unencrypted, and encrypted and unlocked) is downgraded; `{:?}`, `to_bytes()` and the in-memory entries hold no secret, and both scripts are still watched.
- **Load rules.** `from_bytes` of an encrypted envelope pairs each kind-2 record with exactly one `SealedPrivateKey` entry whose primary script is the record's id (`sha256(primary script)`, §2.6). A missing record, a duplicate id, or an orphan record with no entry → `PersistenceError::Corrupt`. An unencrypted envelope containing kind-2 records, or an encrypted one with a `PrivateKey` entry → `Corrupt`.

**Wiping and `Debug` (D35, D39, D41).**
- Only private key material is wiped: the 32 secret bytes of `ImportedKind::PrivateKey`. `ScriptBuf`, `PublicKey`, labels, births, sealed ciphertext and the map's structure are public and are left alone; the map is not cleared by `zeroize()`.
- `Zeroizing<[u8; 32]>` wipes its own bytes on drop, so an entry that is removed or replaced (for example on `UpgradedToSpendable` or sealing) is wiped even when the container lives on; `Drop for ImportedKeys` covers the container as a whole. No `Drop` on `ImportedEntry`/`ImportedKind`, so they can still be matched and moved.
- WIF input is parsed straight into the `Zeroizing` buffer; `import_private_key` takes `&PrivateKey` (whose own memory the caller owns, as today) and keeps no other copy.
- `Wallet`'s existing manual `Zeroize` gains `self.imported.zeroize()`, so wiping a wallet wipes its imported keys too.
- `Zeroizing<[u8; 32]>`'s own `Debug` already prints `Zeroizing { .. }` in zeroize 1.9.1 (`lib.rs:601-605`). The manual redacted `Debug` is kept as defence in depth: it does not depend on a dependency's formatting choice, and it also fixes what the entries print (D39).
- **Tests (WL1-5).**
  - A `Wallet` with two imported private keys (one compressed, one uncompressed) is formatted with `{:?}` and `{:#?}`; the output contains neither key's 32 bytes as hex, nor either WIF string, nor the bytes as a Rust array literal. A second test calls `ImportedKeys::zeroize()` and checks that each secret buffer is all zeros while every script and public key is still present.
  - **Sealed lifecycle (D41):** import two keys → `encrypt` → `lock` → scan `{:?}` and the in-memory entries (no secret bytes, every entry is `SealedPrivateKey`) → `unlock` → `imported_signing_key` signs, and the signature verifies.
  - **Import while encrypted:** `encrypt` → `unlock` → `import_private_key` → `to_bytes` → `from_bytes` → `unlock` → sign with the new key. Importing while locked and while mixing-only returns the two `Locked` errors and changes nothing.
  - **Load rules:** a file with a kind-2 record removed, duplicated, or added without an entry is refused with `Corrupt`.

**Where `ImportedKeys` lives.** It is a new private field on `Wallet` (`imported`, read through `Wallet::imported()`). It is not part of the legacy bincode layout. §2.8 explains how the field is persisted without changing existing bytes.

**Rules mirroring Core:**
- **Wallet kinds that accept imports.**
  - `WalletType::WatchOnly` refuses private-key imports. This is Core's `DISABLE_PRIVATE_KEYS` −4 (`backup.cpp:140-142`), surfaced as `ImportError::PrivateKeysDisabled`.
  - `Seedless`, `Mnemonic*`, `Seed` and `ExtendedPrivKey` accept everything.
  - `ExternalSignable` accepts only watch-only imports.
- **Importing a private key for a script already watch-only** moves the entry to spendable (`UpgradedToSpendable`). This is the ownership upgrade Core makes when it later gains the key.
- **Importing the same key or script again** is `AlreadyPresent` and does not change the label. Applying labels (Core `apply_label`) is the RPC layer's job, in WL2b.
- **Compression** is preserved: an uncompressed WIF imports the uncompressed P2PKH script (D9).
- **P2PK (D48).** Core's `importprivkey` and `importpubkey` also make the raw-pubkey script `GetScriptForRawPubKey` (P2PK) the wallet's: `importpubkey` adds it as watch-only (`ImportPubKeys`, `scriptpubkeyman.cpp:1763-1781`), and IsMine's `TxoutType::PUBKEY` case returns SPENDABLE when the key is held (`scriptpubkeyman.cpp:101-107`). WL1 records P2PK(pubkey) as an alias of every key and pubkey entry: an output paying it is owned, spendable when the entry holds a private key (plain or sealed) and watch-only otherwise.
- **Which RPC applies to which wallet kind** (descriptor vs legacy, Core's −4 per kind) is WL2a/WL2b policy. WL1 only refuses what is impossible for the wallet type.

**Managed side (D81, D83).** The imported managed accounts are derived state. They are rebuilt from `Wallet::imported()`, the only authority, and never serialised as part of the collection, so `ManagedWalletInfo`'s serde form is unchanged.
- `ManagedAccountType` gains `ImportedKeys { scripts: ImportedScriptPool }` and `ImportedWatchOnly { scripts: ImportedScriptPool }`. `ImportedScriptPool` is a fixed script set with `used` flags and reverse lookups; it has no gap limit and no derivation. `ManagedAccountType` derives `Serialize`/`Deserialize` and bincode `Encode`/`Decode` (`managed_account/managed_account_type.rs:17-21`), so `ImportedScriptPool` implements them too; the two variants are appended, so every existing variant index is unchanged (D97).
- `ManagedAccountCollection` gains `#[cfg_attr(feature = "serde", serde(skip))] imported_keys: Option<ManagedCoreFundsAccount>` and `imported_watch_only` with the same attribute. Every serde format writes exactly what it wrote before WL1-5 and reads old data unchanged.
  - WL1 adds no persisted field to `ManagedWalletInfo`. Non-self-describing formats (bincode-serde, `platform_value::Value`) cannot default a missing field and need a versioned migration for one (`managed_wallet_info/mod.rs:116-121`).
  - Platform persists the managed side field by field and rebuilds it with `from_wallet` (`rs-platform-wallet-ffi` `persistence.rs:5367-5369`), so it needs no schema change.
- **Rebuild (D81).** The required `WalletInfoInterface::sync_imported_pools(&mut self, imported: &ImportedKeys)` reconciles the two accounts with the wallet's entries:
  - it creates them when entries exist and adds every missing script with its P2PK alias;
  - it moves a script whose entry changed kind (D70) to the other account, with its UTXOs and transaction records;
  - it drops every script that no entry holds, with its UTXOs, so no orphan can count in a balance;
  - it bumps the monitor revision on any change.

  It runs in `ManagedWalletInfo::from_wallet`, so every restore through `from_wallet` monitors the imports (Platform's restore, the D42 re-register path). It also runs at every `WalletManager` site that inserts a wallet with its managed info (`insert_wallet`, `accessors.rs:63-69`; `import_wallet_from_bytes` and the `create_wallet_*` sites, `lib.rs:306-559`), because a deserialised managed info has no imported accounts. Finally it runs after every import, `remove_imported` (D85) and `downgrade_to_external_signable` (D70) through `WalletManager`.

  **Cursor seed in `from_wallet` (D90).** After the sync, `from_wallet` sets `synced_height` and `last_processed_height` to `min(birth_height, earliest import birth) − 1`, saturating, with births resolved as in the import wrappers below (`Genesis` and unresolved `Time` → 0). `metadata.birth_height` stays the caller's HD birth, so D81's account-add floor is unchanged. This is Core's rescan from the first key time for a wallet loaded without a usable locator, not a load-time check. A loader that restores a persisted cursor overwrites it afterwards (Platform, `rs-platform-wallet-ffi` `persistence.rs:5367-5369`; WL2a), and `insert_wallet` with a caller-built info keeps the caller's cursor.
- **Funds state after a reload (D82).** A rebuilt account starts with no UTXOs or transaction records.
  - Core persists its wallet transactions and recomputes ownership from the key store at load. WL2a does the same: after the rebuild it replays its `txs` records (A1 §2.20) in block order, each with its stored context, through the existing transaction-checking path, then calls `update_balance`. All of this completes before the wallet manager processes any block or mempool transaction and before any build or RPC is served, as Core loads wallet transactions before it attaches the wallet to the chain (§4.2, D91).
  - A consumer that persists only `ManagedWalletInfo`'s serde form and holds imports calls `rewind_scan_cursor` with the earliest import birth after each load, the equivalent of Core's `rescanblockchain`. The import wrappers' API docs say so (risk 15). This path is order-safe already, since its rescan re-delivers every spend.
- **Routing.** `AccountTypeToCheck` and `CoreAccountTypeMatch` gain the two kinds. For every transaction type that today checks `StandardBIP44`, `get_relevant_account_types` also checks the imported kinds (`transaction_router/mod.rs:105`).
- **Balances.** `ManagedWalletInfo` balance totals keep the watch-only account out of the spendable total and expose a computed `watch_only()` accessor (the `ImportedWatchOnly` account's balance), as Core's `getbalances.watchonly` does. It is not a field, because a new `WalletCoreBalance` field would change the serde form (D83, D93). The persisted cached `balance` still counts imported funds from before a reload until WL2a's replay ends with `update_balance` (D91).
- **Monitoring.** `WalletInterface::monitored_script_pubkeys_for` includes both imported sets and their P2PK aliases; `monitored_addresses` includes every imported script that has an address form. dash-spv's compact-filter scan calls `scan_script_pubkeys_for` (`filters/manager.rs:747, 914`), which defaults to the monitored set (`wallet_info_interface.rs:101-103`), so it sees the imported scripts too. Raw scripts with no address form (including the P2PK aliases) are returned by a new **required** `WalletInterface::monitored_extra_scripts() -> Vec<ScriptBuf>`, which `WalletManager` aggregates from the required per-wallet `WalletInfoInterface::monitored_extra_scripts()` and which dash-spv's mempool filter reads (§4.1, D49, D78). `earliest_required_height` takes, per wallet, the lower of `birth_height()` and the earliest birth in `Wallet::imported()` (`Genesis` is 0; `Time` is resolved by the caller, D11, else 0). `WalletManager` reads it from the wallet itself (`process_block.rs:346-348`), so no managed field holds it (D81).
- **Monitor revision (D64).** `ManagedAccountCollection::all_accounts()`/`all_accounts_mut()` include `imported_keys` and `imported_watch_only`, and inserting a script into an `ImportedScriptPool` bumps that account's monitor revision, so `ManagedWalletInfo::monitor_revision` (`wallet_info_interface.rs:614-616`) changes on every import and dash-spv rebuilds its filters (§4.1).
- **History scan after an import (D65, D81).** There is one scan cursor, as in Core: an import rescans from its birth, and nothing else records it.
  - Every `WalletManager` import wrapper records the import's birth height on the entry (`Genesis` → 0; `Time` → 0 unless the caller resolved it to `Height`, D11).
  - It then calls the required `WalletInfoInterface::rewind_scan_cursor(height)` on that wallet's managed info (D78). This is the #649 rewind (`rewind_sync_checkpoint_for_new_account`, `managed_wallet_info/mod.rs:401-410`) generalised to take the target: `synced_height` becomes `min(synced_height, height − 1)` (saturating), and `account_generation` is bumped.
  - **Why that suffices.** An in-flight scan batch cannot certify coverage that never tested the imported scripts (the commit-time guard at `filters/manager.rs:605`), and dash-spv backfills the wallet through its existing behind-wallet path.
  - **The HD birth is untouched.** `metadata.birth_height` does not change, so the rewind that every later account add runs (`managed_accounts.rs:83, 118, 166, 202, 251, 292`) still floors at the HD birth. A new HD account has no history before it, and a wallet with a `Genesis` import does not rescan from 0 on every contact add.
  - An `AlreadyPresent` import changes nothing.
- **Persistence order (WL2a; the only ordering contract, D81).** WL2a persists the lowered cursor first and the new imported entry second, both before the import call returns (§4.2). It holds the wallet lock from the in-memory import through both writes, as Core holds `cs_wallet`, so no other wallet-file write (a label, an account add) can persist the entry before the cursor (D94).
  - A crash before either write means the import never happened; the caller retries.
  - A crash between the two writes leaves a lowered cursor and no new entry. The wallet rescans once and finds nothing new, which is harmless, and the caller retries the import.
  - Once both are written, the import is complete. The imported accounts are rebuilt from the entry at every load, so no pool state can be lost or orphaned.

  No load-time check is needed, as in Core (D81 supersedes D76).
- **Tests (WL1-5, D81–D83, D85).**
  - **Crash between the writes.** Persist the rewound managed info but not the wallet entry, then reload. No entry exists and the wallet rescans once. A retried import returns `Added` and finds the old payment.
  - **Restore through `from_wallet`** of a wallet holding an imported key and a raw script. Both are monitored, the raw script is in `monitored_extra_scripts`, and a new mempool payment to the key is seen. The old payment below the given `birth_height` is found too (D90): HD birth 1000, the key imported with `Height(500)` and paid at 600, the wallet re-registered with `import_wallet_from_bytes(bytes, 1000)`, and the payment at 600 is in the balance.
  - **Insert with a deserialised managed info.** `insert_wallet` with a managed info read back from serde (no imported accounts) rebuilds them.
  - **Removal.** `remove_imported` drops the entry's scripts from monitoring and its UTXOs from the balance, and changes the monitor revision.
  - **Kind change.** After `WalletManager::downgrade_to_external_signable`, the converted scripts and their UTXOs sit in `ImportedWatchOnly` (D70).
  - **Serde form.** A managed info with imported accounts serialises byte-equal to the same info with the two fields set to `None` and `balance` untouched (§2.4, D93).
- **`WalletManager` wrappers:** `import_private_key`, `import_public_key`, `import_address`, `import_raw_script` (each forwards to the `Wallet` method and its gates, then calls the required `WalletInfoInterface::rewind_scan_cursor` and `sync_imported_pools`, D78, D81), `remove_imported(wallet_id, script)` (D85), `imported_entries(wallet_id)`. Their API docs state the anchor limit (D77, D95): on an SPV client, history below the header anchor is never scanned, so a `Genesis`/`Time` import (or any birth below the anchor) misses it. A fresh dash-spv client anchors at `max(earliest_required_height, hd_wallet_sync_floor())` (`dash-spv/src/client/lifecycle.rs:113-120`), which on mainnet is the HD activation height, so by default every mainnet import born before HD activation (an old paper-wallet sweep, §4.3) is below the anchor. They also state the reload duty of D82. Whether an address with `p2sh=true` is an error (Core −5) is checked by the RPC layer, since `import_address` takes no `p2sh` flag.

**Seedless wallets.** `WalletType::Seedless` is the wallet-level "seedless" case: no HD root, no `AccountCollection` accounts by default (a caller may add xpub accounts, which are `CallerSupplied`, D60), and `imported`.
- **Change on seedless spends (D74, D84).** Core's blank wallet has no keypool, so its spends need an explicit change address. WL1 enforces this where change is chosen.
  - `TransactionBuilder::fund_from` takes change from the first funding account when none is set (`transaction_builder.rs:234-236`). Every wallet-level spend reaches it through `ManagedWalletInfo::fund`, which calls `add_funding` per account (`transaction_building.rs:324`) and is shared by `funded_builder` and the asset-lock builder.
  - `fund` sets the caller's change address on the builder first. For a `Seedless` wallet with none, it returns the existing `BuilderError::NoChangeAddress` before it reserves or selects anything, so a `CallerSupplied` account never supplies change.
  - The three entry points `build_and_sign_transaction`, `build_and_sign_transaction_with_signer` and `build_unsigned_transaction` (`transaction_building.rs:123-170`) gain a trailing `change_address: Option<Address>`, Core's `changeAddress`. `None` keeps today's behaviour for every other wallet type. WL2b's `fundrawtransaction`/`send*` pass `changeAddress` through. Both asset-lock builders reach `fund` (`asset_lock_builder.rs:405`), so the guard refuses a seedless wallet there too; `build_asset_lock_with_signer` has no root-key check of its own (`:548-576`, D92).
  - `ManagedWalletInfo::next_change_address` (`managed_wallet_info/mod.rs:418-433`) keeps its `Option<Address>` signature and returns `None` for `Seedless`, and so does `WalletManager::next_change_address` (`lib.rs:759-769`).
  - The FFI getter `managed_wallet_get_next_bip44_change_address` (`key-wallet-ffi/src/managed_wallet.rs:96-122`) returns null with `InvalidState` for a `Seedless` wallet. Today's FFI constructors and the legacy decoder cannot produce one, so this is defence in depth. The FFI build calls pass `None`.
  - **Tests (WL1-5).** Take a `Seedless` wallet holding a caller-supplied `Standard` account with funds:
    - `build_unsigned_transaction(…, None)` fails with `NoChangeAddress` and reserves nothing;
    - `build_unsigned_transaction(…, Some(addr))` pays change to `addr`, and the account's internal pool index does not move;
    - `next_change_address` returns `None`;
    - the FFI getter returns null with `InvalidState`.

**Watch-only wallets** keep their existing representation (`WalletType::WatchOnly` + xpub accounts) and can additionally hold `ImportedWatchOnly` entries. Core's descriptor watch-only wallet is `disable_private_keys` + `importdescriptors addr()/raw()/pkh(xpub)`.

### 2.6 Encryption at rest (`crypto/`, `wallet/encryption.rs`, feature `encryption`)

**Model (D6, D7).** The model has two levels, as Core's `mkey` and the desktop `dw-vault` slot P do:

```
passphrase ──argon2id(salt, m, t, p)──► KEK (32 B)
KEK ──XChaCha20-Poly1305(nonce_w, aad_w)──► wraps MK (random 32 B)
MK  ──XChaCha20-Poly1305(nonce_i, aad_i)──► seals each secret record i
```

**`MasterKeyRecord`.** This is stored once per wallet and re-wrapped on passphrase change. Its byte encoding is fixed, little-endian and versioned:

| Offset | Size | Field | Value |
|---|---|---|---|
| 0 | 4 | magic | `b"KWMK"` |
| 4 | 1 | format version | `1` |
| 5 | 1 | kdf id | `1` = argon2id v0x13 |
| 6 | 4 | `m_kib` | u32 |
| 10 | 4 | `t` | u32 |
| 14 | 4 | `p` | u32 |
| 18 | 16 | salt | random per wrap |
| 34 | 24 | nonce | random per wrap |
| 58 | 48 | wrapped MK | 32 B ciphertext ‖ 16 B tag |

The wrap AAD is `b"key-wallet/mk/v1" ‖ bytes[0..34] ‖ wallet_id`: every header byte **before the nonce** (magic, version, kdf id, parameters, salt) plus the wallet ID. The nonce (bytes 34–57) is not in the AAD; it is authenticated as the AEAD's nonce input, so changing it also fails the tag. Together these stop a downgrade by editing the parameters and a transplant to another wallet.

**`SealedRecord`.** There is one per secret: mnemonic, passphrase, seed, root xprv, and each imported private key.

| Offset | Size | Field |
|---|---|---|
| 0 | 4 | magic `b"KWSR"` |
| 4 | 1 | format version `1` |
| 5 | 1 | record kind (`1` wallet-type secret, `2` imported key, `3`+ reserved for WL4/WL5) |
| 6 | 2 | reserved, must be 0 |
| 8 | 32 | record id (kind 1: zero; kind 2: `sha256(script_pubkey)`) |
| 40 | 24 | nonce, random |
| 64 | n+16 | ciphertext ‖ tag |

The record AAD is `b"key-wallet/sr/v1" ‖ bytes[0..40] ‖ wallet_id`: every header byte before the nonce (magic, version, kind, reserved, record id) plus the wallet ID; the nonce (bytes 40–63) is authenticated as the AEAD nonce input. Records cannot be swapped between wallets, kinds or ids, or have their nonce altered, without failing authentication.
- **Kind-1 plaintext.** `encoding ‖ records_digest (32 B) ‖ zero padding`. The encoding is the §2.8 codec of the cleartext `WalletType`: variant 0, 1, 2, 5 or 6. Variant 6 (`Seedless`) is the tag alone with an empty payload, so every encrypted wallet, seedless included, has exactly one kind-1 record and the envelope rule of §2.8 is unchanged (D33). The codec is self-delimiting, so the reader knows where the digest starts. Zero bytes pad the whole plaintext to at least 1024 bytes and above that to the next multiple of 256 (D63, amending D54). Measured with the existing codecs (Opus r2), a 24-word variant-5 wallet with a 256-byte passphrase (Core's legacy-path limit) needs about 574 bytes with the digest, so every English wallet with a passphrase of up to 256 bytes lands in the one 1024-byte bucket; only longer passphrases (Core's descriptor path accepts them, D5) or long non-English phrases reach a larger bucket.
- **No reallocation (D45, D63).** The writer first runs bincode's size pass over the `WalletType` (encoding into a counting writer, which stores no bytes), computes `max(1024, round_up_256(size + 32))`, allocates a `Zeroizing<Vec<u8>>` with exactly that capacity, then encodes into it. The buffer never grows, so it leaves no unwiped copy.
- **Kind-2 plaintext.** `secret(32) ‖ compressed(1)`. The record id is `sha256(primary script)`, the entry's P2PKH script (§2.5).
- **Records digest (D43, D61).** `records_digest = sha256(b"key-wallet/rs/v1" ‖ count u32 LE ‖ the kind-2 record ids, sorted ascending)`. It sits inside the kind-1 plaintext, so the kind-1 record authenticates the number and set of kind-2 records, and the two failure modes stay distinguishable: a kind-1 tag failure is `Corrupt`; a kind-1 record that opens but whose stored digest differs from the digest recomputed from the file's kind-2 records is `Tampered(RecordSet)`. The only change to the kind-2 set while the MK is held is `import_private_key` while Full (including `UpgradedToSpendable`), which reseals kind 1 with a fresh nonce under the existing MK, atomically (D62). `downgrade_to_external_signable` drops kind 1 and every kind-2 record instead (D57). Cost: one 32-byte hash per unlock and per kind-2 change.

Decode checks after unwrapping a kind-1 record: the plaintext variant must match the public `kind`; `Seedless` needs `root_xpub = None` and an empty payload; a variant outside {0, 1, 2, 5, 6}, a plaintext shorter than 1024 bytes or not a multiple of 256, fewer than 32 bytes after the encoding, or a non-zero padding byte is `Corrupt` (the record authenticated, so the writer was buggy, not an attacker). The public-metadata comparisons (root xpub, account keys, imported keys) are the unlock check below (D43). Tests (WL1-6): a seedless kind-1 record decodes and re-encodes byte-equal; each mismatch above fails; a variant-6 plaintext with one non-zero padding byte fails; plaintexts of 768, 1023 and 1025 bytes fail; a 24-word variant-5 wallet with a 256-byte passphrase seals to a 1024-byte plaintext.

**Nonces.**
- Every seal draws 24 random bytes from `getrandom`. At 192 bits, random nonces are safe for the life of a wallet; no counter is kept, so nothing can roll back.
- Re-sealing a record, for example after an upgrade, always draws a fresh nonce.
- A passphrase change re-wraps only the MK, with a fresh salt and nonce. The records stay untouched, as in Core `ChangeWalletPassphrase` (`wallet.cpp:576-617`).

**Versioning.**
- Readers accept `version == 1` only.
- An unknown format version, kdf id or record kind, or a non-zero reserved field, is `CryptoError::UnsupportedFormat`, and the whole file is refused. Nothing is preserved or skipped: a reader that cannot understand every record does not load the wallet, so it can never write a copy that drops records (D30).
- **Forward compatibility.** Any change that adds a record kind or field bumps the envelope version (§2.8) or the record version. Older readers then refuse the file with `UnsupportedFormat` rather than load it partially. A newer reader migrates older files on unlock. WL4/WL5 (record kinds 3+) must bump the version when they start writing new kinds.

**KDF parameters (D8)**

| Constant | m | t | p | Use |
|---|---|---|---|---|
| `KdfParams::DESKTOP_FLOOR` | 256 MiB | 3 | 1 | floor of `KdfPolicy::desktop()`, used by `dashd-wallet` and recommended for the desktop; calibration raises `t` until one derivation takes ≥ 500 ms, capped at `t = 24` |
| `KdfParams::MOBILE_FLOOR` | 64 MiB | 3 | 1 | floor of `KdfPolicy::default()` (mobile-safe library default, D28); same calibration rule |
| `KdfParams::CEILING` | 4 GiB | 64 | 16 | absolute sanity ceiling: no policy or override can raise the unauthenticated budget above it. On a target where 4 GiB is more than an `isize::MAX`-byte allocation (32-bit), `m` is clamped to the largest addressable whole number of 1 KiB blocks (D45) |
| `KdfParams::TEST` | 64 KiB | 1 | 1 | only with `KdfPolicy::allow_weak_for_tests()`, behind `#[cfg(any(test, feature = "insecure-test-kdf"))]`; nothing forwards that feature, so `test-utils` does not reach it (D58) |

Every floor is far above Core's 25,000 SHA-512 rounds, as FR-WAL-04 requires ("at least as strong as Core's"). `calibrate(target: Duration)` is a pure function of a caller-supplied timer closure, so tests can drive it deterministically. **The floor applies to writes only (D44).** `encrypt`, `change_passphrase` and migrate-on-unlock refuse parameters below the active policy's floor (`CryptoError::WeakKdf`). Reading has no floor: a file is opened if it is within the budget and the ceiling, and its parameters meet argon2's own minimums (`m ≥ 8·p` KiB, `t ≥ 1`, `p ≥ 1`; otherwise `UnsupportedFormat`). A 64 MiB file written under `default()` therefore opens under `desktop()`, and is upgraded on unlock (below).

**Pre-authentication KDF budget (D28).** The KDF parameters in a `MasterKeyRecord` are read *before* anything authenticates them; the AEAD tag can only be checked after argon2id has run. So the reader decides how much work it is willing to do on unauthenticated input:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KdfBudget { pub max_m_kib: u32, pub max_t: u32, pub max_p: u32 }

impl KdfBudget {
    pub const MOBILE: KdfBudget  = KdfBudget { max_m_kib: 64 * 1024,  max_t: 24, max_p: 4 };  // default
    pub const DESKTOP: KdfBudget = KdfBudget { max_m_kib: 256 * 1024, max_t: 24, max_p: 4 };
}

pub struct KdfPolicy { write_floor: KdfParams, budget: KdfBudget /* , weak_allowed (test only, D58) */ }
impl KdfPolicy {
    pub fn default() -> Self;                                    // MOBILE_FLOOR, KdfBudget::MOBILE
    pub fn desktop() -> Self;                                    // DESKTOP_FLOOR, KdfBudget::DESKTOP
    /// Caller-selected maximum, e.g. after asking the user. Err if above KdfParams::CEILING.
    pub fn with_unauthenticated_budget(self, budget: KdfBudget) -> Result<Self, CryptoError>;
    /// The parameters a write under this policy uses: the floor, with `t` raised by calibration.
    pub fn write_params(&self) -> KdfParams;
}
```

- **Check order.** Header parsed → ceiling check (`KdfAboveCeiling`) → budget check (`KdfOverBudget`) → argon2 minimums (`UnsupportedFormat`) → only then argon2id. A file over the budget fails with `CryptoError::KdfOverBudget { required: KdfParams }` **before** the argon2 context or its memory is allocated. There is no floor check on this path (D44).
- **Memory (D45).** argon2 is declared `argon2 = { version = "0.5.3", default-features = false, features = ["zeroize"], optional = true }` (D71): the defaults (`alloc`, `password-hash`, `rand`) would pull in password-hash, base64ct and rand_core, none of which WL1 uses, and `hash_password_into_with_memory` needs no `alloc`. argon2 is built with its `zeroize` feature, and WL1 calls `Argon2::hash_password_into_with_memory` with a block buffer it owns: allocated with `Vec::try_reserve_exact` (a failure is `CryptoError::OutOfMemory`, not an abort) and wiped with `Block::zeroize` on every path before it is freed. `hash_password_into` is not used, because it allocates its own buffer and frees it unwiped (argon2 0.5.3 `lib.rs:229-231`).
- **Migrate on unlock (D44).** After a successful MK unwrap, if the record's parameters are below the policy's write floor in any dimension, `unlock` re-wraps the MK under `policy.write_params()` with a fresh salt and nonce, and reports it in `UnlockOutcome { kdf_upgraded: true }` so the caller persists the wallet. The records and the MK are unchanged. It never re-wraps to weaker parameters: a 256 MiB file opened under `default()` keeps its parameters (if it is within the caller's budget at all). If the re-wrap fails (RNG, memory), the unlock still succeeds with the old record and `kdf_upgraded: false`.
- **Consistency.** Each policy's writer never produces parameters its own budget would refuse: calibration caps `t` at 24 and writes `p = 1`, so a default-written file always opens under the default policy, and a desktop-written file under `desktop()`.
- **Recovery path.** `KdfOverBudget` carries the required parameters, so a consumer can show them ("this wallet needs 1 GiB to open"), and retry with `with_unauthenticated_budget` up to `CEILING`. Above `CEILING` the file is `KdfAboveCeiling`, with no override.
- **What this is not.** A budget bounds the work a crafted file can cause; it does not make that work free. A file at the budget still costs up to one budget-sized derivation per open attempt (≈ 64 MiB × 24 passes by default). Callers that open untrusted files in a loop must rate-limit themselves.
- **Boundary tests (WL1-4),** with a test-scale budget `{ max_m_kib: 128, max_t: 2, max_p: 1 }` so they run fast: parameters exactly at the budget open (with the right passphrase); `m = 129`, `t = 3` and `p = 2` each fail with `KdfOverBudget`, and an instrumented KDF hook proves argon2 was never called; `with_unauthenticated_budget` above `CEILING` errors; a file at `CEILING + 1` in any dimension is `KdfAboveCeiling` even after an override; `default()` opens every file `default()` writes, and `desktop()` every file `desktop()` writes (calibration driven to its `t = 24` cap).
- **Floor tests (WL1-4, D44),** with test-scale floors: a file written under the lower floor opens under the higher-floor policy; `unlock` returns `kdf_upgraded: true`, the MK record now carries the higher parameters, the same passphrase unlocks it again, every `SealedRecord` is byte-identical, and a second unlock returns `kdf_upgraded: false`. A file written under the higher floor and opened under the lower policy is not re-wrapped. `encrypt` and `change_passphrase` below the floor fail with `WeakKdf`. An `#[ignore]`d test does the same with the real 64 MiB → 256 MiB parameters.

**Crypter and lock state**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnlockMode { Full, MixingOnly }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockState { Unencrypted, Locked, UnlockedMixingOnly, Unlocked }   // = Qt WalletModel states

/// What an operation needs (D12). MixingOnly unlock permits only CoinJoin and KeyPoolTopUp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretUse { General, CoinJoin, KeyPoolTopUp, Export, MessageSign }

pub struct EncryptedWallet {
    pub master: MasterKeyRecord,
    pub secret: SealedRecord,                   // kind 1
    pub kind: SecretKind,                       // Mnemonic | MnemonicWithPassphrase | Seed | ExtendedPrivKey | Seedless (kind-1 plaintext = variant 6, D33)
    pub root_xpub: Option<RootExtendedPubKey>,  // public; lets locked wallets derive public keys the same as unlocked (D20, n2)
    unlocked: Option<Unlocked>,                 // transient; never encoded
}
struct Unlocked { mk: Zeroizing<[u8; 32]>, plain: Box<WalletType>, mode: UnlockMode }
// Kind-2 records live in the SealedPrivateKey entries of Wallet::imported (§2.5, D41), not here.

// Required by WalletType's derives (Clone, Debug, Zeroize) on variant 7 (D37).
impl Clone for EncryptedWallet { /* records and public metadata only: the clone is always Locked (D56) */ }
impl Zeroize for EncryptedWallet { /* = lock(): if unlocked, mk.zeroize() and plain.zeroize() explicitly (D45), then drop `unlocked`;
                                       records and public metadata are ciphertext/public and stay */ }
impl Drop for EncryptedWallet { fn drop(&mut self) { self.zeroize() } }
impl fmt::Debug for EncryptedWallet { /* kind, root_xpub, lock state, record lengths; never MK, plaintext or record bytes */ }
// MasterKeyRecord and SealedRecord: derive(Clone, PartialEq, Eq) over their bytes; Debug prints magic, version, kind and length only.
// bincode: EncryptedWallet has a manual codec used only by the envelope writer (§2.8): master (106 B), kind (u8),
// root_xpub (Option). The kind-1 `secret` record is written in the envelope's record section, `unlocked` never.
```

**Clone is locked (D56).** Cloning an encrypted wallet never copies the MK or plaintext; the clone must be unlocked on its own. `WalletManager::create_wallet_*` today clones the wallet on insert (`lib.rs:298-299`); WL1-6 removes that clone (the wallet is moved into the map and the return value built from it). That clone copies a freshly built *unencrypted* wallet, so removing it is cleanup; the guarantee rests on the locked `Clone` impl itself, which every clone (WL2a snapshots, key-wallet-ffi's `wallet_manager_get_wallet`, `wallet_manager.rs:387`) goes through (D69). Test: unlock → clone → the clone is `Locked`, the original `Unlocked`; `{:?}` of both shows no MK byte.

**Trait coverage (D37).** zeroize 1.9.1 has no `Zeroize` impl for `Box<T>` (only `Box<[T]>` and `Box<str>`), but `WalletType`'s derived `Zeroize` still reaches `EncryptedWallet::zeroize` through `Box`'s `DerefMut`, because zeroize_derive 1.5 emits method-call syntax. Every type reachable from `WalletType` and `Wallet` must satisfy their derive set: `Clone`, `Debug`, `Zeroize` (for `WalletType`), serde through the skipped variants (no impl needed) and bincode through the hand-written codec (D26). Neither `WalletType` nor `Wallet` derives `PartialEq`, so none is required. A compile-time assertion in the library itself (WL1-1, extended by each slice that adds a type) pins this. It is a `const` item, not a `#[test]`, so every `cargo check --lib` evaluates it under that build's own features (D53):

```rust
// key-wallet/src/wallet/mod.rs
const _: () = {
    const fn assert_core<T: Clone + core::fmt::Debug + Zeroize>() {}
    #[cfg(feature = "bincode")]
    const fn assert_codec<T: bincode::Encode + bincode::Decode<()> + for<'de> bincode::BorrowDecode<'de, ()>>() {}
    assert_core::<SecretBytes>();                                                     // WL1-1
    assert_core::<DashCoreMnemonic>(); assert_core::<WalletMnemonic>();               // WL1-2
    assert_core::<ImportedKeys>(); assert_core::<ImportedEntry>(); assert_core::<ImportedKind>();   // WL1-5
    assert_core::<EncryptedWallet>();                                                 // WL1-4 (always compiled, D51)
    assert_core::<WalletType>(); assert_core::<Wallet>();                             // WL1-1
    #[cfg(feature = "bincode")] { assert_codec::<SecretBytes>(); assert_codec::<DashCoreMnemonic>(); assert_codec::<WalletMnemonic>();
                                  assert_codec::<WalletType>(); assert_codec::<Wallet>();
                                  assert_codec::<EncryptedWallet>(); }                  // WL1-4 (D88)
};
```

- **Why `BorrowDecode` is required (D80, W1-13).** bincode 2's derived `Decode` on any containing type also derives `BorrowDecode`, and that needs `BorrowDecode` on every field.
- **Rule for every secret type.** `BorrowDecode` delegates to `Decode` and never borrows from the input, so no decoded secret aliases the caller's buffer.
  - It applies to `SecretBytes`, `WalletType` and `Wallet` (WL1-1, `secret.rs`, `wallet/codec.rs`).
  - It applies equally to every later type: `DashCoreMnemonic`, `WalletMnemonic`, `EncryptedWallet`, `SealedRecord`, `MasterKeyRecord`.
  - The `const` item asserts it for every one of them that has a bincode codec: WL1-4 adds `EncryptedWallet`, and `SealedRecord`/`MasterKeyRecord` join if they ever get bincode impls (D88).

**Per-feature matrix (D53).** key-wallet's dev-dependency on itself (`Cargo.toml:56`) turns on `test-utils, serde, bincode, eddsa, bls` for every test build, so `cargo test --no-default-features` does not test a no-default build. The matrix is therefore made of library checks, which the self dev-dependency does not touch: `cargo check -p key-wallet --lib` with `--no-default-features`, default features, `--features encryption`, `--features bincode,bls,eddsa` (key-wallet-ffi's default set; `bincode` implies `serde`) and `--all-features`. Each one evaluates the `const` assertion and every `cfg` arm. Runtime tests run under the unified dev-dependency set; removing the self dev-dependency is BL-104's job, not WL1's. The same slices add a redacted-`Debug` test for `DashCoreMnemonic`, `WalletMnemonic::DashCore` and an unlocked `EncryptedWallet` to the scan test of §2.3 (no phrase word, MK byte, plaintext byte or chain-code byte in `{:?}`/`{:#?}`, D40), and a codec test: a `DashCoreMnemonic` with a Core-only checksum (accepted by Core, rejected by strict BIP39) round-trips, and bytes holding a phrase Core rejects fail to decode.

The `Wallet` API. `lock_state` and `require` are always compiled (`wallet/lock.rs`, D52): an unencrypted wallet is `Unencrypted` and `require` returns `Ok`; without feature `encryption` an `Encrypted` wallet is `Locked` and `require` returns `Err(Locked)`. The rest is in feature `encryption`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnlockOutcome { pub kdf_upgraded: bool }   // true: the MK was re-wrapped; persist the wallet (D44)

impl Wallet {
    /// encryptwallet: seal every secret (wallet type + imported keys), switch to Encrypted, end Locked.
    /// Refuses WatchOnly/ExternalSignable (Core -16 "wallet does not contain private keys") and
    /// already-encrypted wallets (Core -15). Writes at policy.write_params(); `kdf` below the floor is WeakKdf.
    pub fn encrypt(&mut self, passphrase: &[u8], kdf: KdfParams, policy: &KdfPolicy) -> Result<(), CryptoError>;
    /// walletpassphrase, in Core's order (D46): MixingOnly requested while Full → AlreadyUnlocked (-17);
    /// empty passphrase → EmptyPassphrase (-8); Full requested while Full → Ok with no verification
    /// (Core Unlock returns true when !IsLocked(), wallet.cpp:4190-4191); from Locked or MixingOnly
    /// (either mode requested) → KDF, MK unwrap (IncorrectPassphrase), the check below, then the mode.
    pub fn unlock(&mut self, passphrase: &[u8], mode: UnlockMode, policy: &KdfPolicy) -> Result<UnlockOutcome, CryptoError>;
    /// walletlock: zeroize MK and plaintext from any state; no-op on unencrypted (caller maps to -15).
    pub fn lock(&mut self);
    /// Core Lock(fAllowMixing=true) (wallet.cpp:4168-4186): Full → MixingOnly keeping the MK;
    /// MixingOnly stays; Locked stays Locked. No-op on unencrypted.
    pub fn lock_to_mixing_only(&mut self);
    /// walletpassphrasechange (wallet.cpp:576-617): remember was_locked = (state == Locked), Core's IsLocked(true); lock();
    /// unlock Full with `old` (wrong → IncorrectPassphrase, and the wallet stays Locked); re-wrap the
    /// MK with fresh salt/nonce at `kdf` (WeakKdf below the floor); if was_locked, lock().
    /// End state: Locked → Locked, MixingOnly → Full, Full → Full, as Core. The MK and records are unchanged (D55).
    pub fn change_passphrase(&mut self, old: &[u8], new: &[u8], kdf: KdfParams, policy: &KdfPolicy) -> Result<(), CryptoError>;
}
impl Wallet {                                           // always compiled (D52)
    pub fn lock_state(&self) -> LockState;
    /// Gate used by every secret-reading path (derive_private_key, signing, export, message signing).
    pub fn require(&self, use_: SecretUse) -> Result<(), CryptoError>;
}
```

**Lock-state tests (WL1-6, D46),** one per Core case: Full + `unlock(wrong, Full)` → `Ok`, still Full (no verification, as Core); Full + `unlock(_, MixingOnly)` → `AlreadyUnlocked`; MixingOnly + `unlock(right, MixingOnly)` → `Ok`, MixingOnly; MixingOnly + `unlock(wrong, MixingOnly)` → `IncorrectPassphrase`, still MixingOnly (Core leaves the state unchanged on a failed re-verify); MixingOnly + `unlock(right, Full)` → Full; Full + `lock_to_mixing_only` → MixingOnly with the MK kept (a CoinJoin `require` passes, `General` fails); Locked + `lock_to_mixing_only` → Locked; `change_passphrase` from each of Locked, MixingOnly and Full → Locked, Full, Full; a wrong old passphrase from Full → `IncorrectPassphrase` and Locked.

Each Core RPC error maps to a `CryptoError` variant that the WL2b RPC layer translates:

```rust
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum CryptoError {
    #[error("wallet is not encrypted")] NotEncrypted,                    // -15
    #[error("wallet is already encrypted")] AlreadyEncrypted,            // -15
    #[error("wallet does not contain private keys")] NoPrivateKeys,      // -16
    #[error("passphrase cannot be empty")] EmptyPassphrase,              // -8
    #[error("the passphrase entered was incorrect")] IncorrectPassphrase,// -14
    #[error("wallet is already fully unlocked")] AlreadyUnlocked,        // -17
    #[error("wallet is locked")] Locked,                                 // -13
    #[error("wallet is unlocked for mixing only")] MixingOnly,           // -13 (EnsureWalletIsUnlocked)
    #[error("key derivation parameters below policy floor")] WeakKdf,
    #[error("key derivation needs more than the unauthenticated budget")] KdfOverBudget { required: KdfParams },
    #[error("key derivation parameters above the absolute ceiling")] KdfAboveCeiling,
    #[error("an account does not match its slot or the wallet's own derivation ({0})")] ForeignAccountKey(AccountSlot), // encrypt precondition (D60, D72)
    #[error("unsupported encrypted format")] UnsupportedFormat,
    #[error("encrypted data is corrupt or was tampered with")] Corrupt, // AEAD failure on a record (not on MK unwrap)
    #[error("wallet public data does not match its sealed secrets ({0})")] Tampered(TamperKind), // unlock check (D43)
    #[error("not enough memory for key derivation")] OutOfMemory,       // argon2 buffer try_reserve failed (D45)
    #[error("random number generator failed")] Rng,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TamperKind { RootXpub, AccountXpub(AccountSlot), SpecialAccountKey(AccountSlot), ImportedKey, RecordSet }   // the slot names the account (D73)
// Display for TamperKind and AccountSlot, and AccountSlot's manual Debug, print the slot kind and its numbers only (D86).
```

**What errors print (D86).** `AccountSlot` has a manual `Debug` and a `Display` that print the slot kind and its numeric fields only: `StandardBip44(0)`, `DashpayReceival(index 2)`, `PlatformPayment(account 0, key class 1)`, `ProviderOperatorKeys`. They never print a `DashpayAccountKey`'s identity ids. Those ids are public Platform data, but §5.1 promises errors with positions and counts only. `TamperKind`'s `Display` uses the slot's, and both error messages above use `{0}`. Test (WL1-6): `Tampered` and `ForeignAccountKey` for a DashPay slot format through `{}`, `{:?}` and `{:#?}` without either id's hex.

**Key check.**
- A wrong passphrase is detected by the AEAD tag on the MK unwrap. That gives `IncorrectPassphrase`, with no separate verifier hash.
- A tag failure on any record after a successful MK unwrap is `Corrupt`, never `IncorrectPassphrase`. A kind-1 record that opens but whose stored records digest differs from the recomputed one is `Tampered(RecordSet)` (D61). Both leave the wallet locked.
- **Unlock check (D43).** On every unlock that verifies the passphrase (both modes), before the state changes:
  1. `root_xpub` equals the root key derived from the kind-1 plaintext (`None` for `Seedless`) → else `RootXpub`.
  2. **Slot consistency (D72).** Every account in `AccountCollection` is checked against its slot (`AccountSlot`, below). Its stored `account_type` must equal the slot's type and its `network` the wallet's, else `AccountXpub(slot)`, or `SpecialAccountKey(slot)` for the BLS/EdDSA slots.
     Then every **self-derived** slot (origin below) is re-derived from the plaintext along the slot's own derivation path (`slot.account_type().derivation_path(network)`, never the stored field) and compared:
     - xpub accounts from the root, else `AccountXpub(slot)`;
     - BLS and EdDSA special accounts (`ProviderOperatorKeys`, `ProviderPlatformKeys`) from the seed, else `SpecialAccountKey(slot)`.
     External and caller-supplied slots are not re-derived.
  3. Every kind-2 record is decrypted; the public key computed from its secret, with its `compressed` flag, must equal the entry's `pubkey`, and the entry's primary script and alias must equal P2PKH(pubkey) and P2PK(pubkey) → else `ImportedKey`.
  On a mismatch: `Err(Tampered(kind))`, the MK and every decrypted buffer are wiped, and the wallet stays (or becomes) `Locked`. The check is linear in the number of accounts and imported keys and runs after the KDF, so its cost is small next to argon2.
- **Account origin (D60).** `AccountCollection` legitimately holds keys the wallet did not derive: `add_account(type, Some(xpub))` stores the caller's xpub unchecked (`accounts.rs:29-40`, called with caller xpubs by key-wallet-ffi `wallet.rs:504, 617, 739`); Platform adds a contact's xpub as `DashpayExternalAccount` to the user's own HD wallet (`rs-platform-wallet` `contacts.rs:568-569`); `add_bls_account`/`add_eddsa_account` take an explicit seed, the only way to give an `ExtendedPrivKey` wallet those accounts (`accounts.rs:90-120, 155-185`). The origin is **computed** from the account's **slot** and the sealed `SecretKind`, never stored (D72).
  - The slot is the map that holds the account plus its key, or the single-account field.
  - The account's stored `account_type` is file data that a tamperer controls, so neither the origin nor the derivation path is ever read from it.
  - Consumers address accounts by slot (`get_bip44_account(i)` is `standard_bip44_accounts.get(&i)`, `wallet/helper.rs:14-16`; the managed accounts are built per slot, `managed_account_collection.rs:380-388`), so the slot is what the check must authenticate.
  - Every slot determines its `AccountType` completely, because `DashpayAccountKey` and `PlatformPaymentAccountKey` carry every field (`account_collection.rs:25-40`).
  ```rust
  /// Where an account sits in `AccountCollection`. One variant per map (with its key)
  /// and per single-account field; e.g. StandardBip44(i) ↔ standard_bip44_accounts[i].
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum AccountSlot {
      StandardBip44(u32), StandardBip32(u32), CoinJoin(u32), IdentityTopUp(u32),
      DashpayReceival(DashpayAccountKey), DashpayExternal(DashpayAccountKey),
      PlatformPayment(PlatformPaymentAccountKey),
      IdentityRegistration, IdentityTopUpNotBound, IdentityInvitation,
      AssetLockAddressTopUp, AssetLockShieldedAddressTopUp,
      ProviderVotingKeys, ProviderOwnerKeys, ProviderOperatorKeys, ProviderPlatformKeys,
  }
  impl AccountSlot { pub fn account_type(&self) -> AccountType; }        // total; the slot's type
  impl AccountCollection { pub fn slots(&self) -> impl Iterator<Item = AccountSlot> + '_; }   // occupied slots

  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum AccountOrigin {
      /// Someone else's key by definition: the DashpayExternal slot (a contact's xpub).
      External,
      /// A key the wallet cannot derive: every slot of a Seedless, WatchOnly or ExternalSignable
      /// wallet, and ProviderOperatorKeys/ProviderPlatformKeys of an ExtendedPrivKey (variant 2) wallet.
      CallerSupplied,
      /// Everything else: derivable from the root (xpub slots) or the seed (BLS/EdDSA slots).
      SelfDerived,
  }
  impl Wallet { pub fn account_origin(&self, slot: AccountSlot) -> AccountOrigin; }   // always compiled
  ```
  - **Defined for every wallet kind (n4).** `WatchOnly` and `ExternalSignable` hold no secret and are never encrypted; every slot but `DashpayExternal` is `CallerSupplied` for them. The imported kinds (`AccountType::{ImportedKeys, ImportedWatchOnly}`) are not stored in `AccountCollection` (D23), so they have no slot and no origin; their keys are checked by check 3.
  - **Unlock** compares only `SelfDerived` accounts. `External` and `CallerSupplied` accounts are unauthenticated public data (§5.1).
  - **Adding accounts to an `Encrypted` wallet.** The origin is that of the slot the account would occupy. `External`: allowed in any lock state (Platform adds contacts while locked). `SelfDerived`: needs `Full`; with `Some(xpub)` (or an explicit BLS/EdDSA seed) the supplied key must equal the wallet's own derivation, else `Error::InvalidParameter("account key does not match the wallet's derivation")`; while locked, `Error::InvalidState("wallet is locked")` (D17). `CallerSupplied`: refused for every account type that can hold or receive funds (all but the four `Provider*Keys` kinds), since nothing can validate it; the provider-key kinds hold no funds and are allowed in any lock state.
  - **Direct field writes (D73).** These gates cover `add_account`, `add_bls_account` and `add_eddsa_account`. `Wallet.accounts`, its maps and `AccountCollection::insert*` stay public, so a caller can still write an account directly; that is outside the guarantee.
    - If such an account (or a tampered one) makes unlock fail, the error names the slot (`Tampered(AccountXpub(slot))` or `SpecialAccountKey(slot)`).
    - The caller removes that account through the public field and unlocks again.
    - For a `SelfDerived` slot (BIP44 account 0, say), removing it loses the user's account, so after unlocking `Full` the caller re-adds it with `add_account(type, None)`. That is the `SelfDerived` path, which derives the correct xpub from the root, and WL2a rebuilds the slot's managed account (D75, D87).
    - Today only key-wallet itself writes the field; Platform adds accounts through `add_account` and removes them through the field (`rs-platform-wallet` `asset_lock/build.rs:684`).
  - **`encrypt` precondition.** Before sealing, `encrypt` runs check 2 over the unencrypted wallet: slot consistency, then the self-derived comparison. An account whose stored type or network disagrees with its slot, or a `SelfDerived` slot whose key differs from the wallet's own derivation (possible today, since unencrypted `add_account(Some)` is unchecked), makes `encrypt` fail with `CryptoError::ForeignAccountKey(slot)` and change nothing; the caller removes or re-adds that account first. Unencrypted wallets keep today's unchecked `add_account`.
  - **`upgrade_to_hd*` precondition.** On upgrade a `Seedless` wallet's `CallerSupplied` accounts would become `SelfDerived` and fail the check, so both upgrade calls refuse a `Seedless` wallet holding any account other than `External` with `UpgradeError::CallerSuppliedAccounts`, checked in step 1 after Core's three checks (lock state untouched).
- **Tests (WL1-6):**
  - For each `TamperKind`, edit a `to_bytes` output (swap a self-derived BIP44 account xpub for another wallet's; change `root_xpub`; replace an imported entry's pubkey; drop one kind-2 record and its entry together), `from_bytes` it, `unlock` → `Tampered(kind)`, `lock_state() == Locked`, `require(General)` fails. A flipped byte in the kind-1 ciphertext → `Corrupt` (D61).
  - **Origins (D60):** an encrypted HD wallet with a `DashpayExternalAccount` holding a contact's xpub unlocks, and a second contact account added while `Locked` keeps it unlockable; an encrypted variant-2 wallet with an explicit-seed `ProviderOperatorKeys` account unlocks; an encrypted `Seedless` wallet with a `Standard` xpub account added before `encrypt` unlocks; `encrypt` of a variant-0 wallet holding a foreign-xpub BIP44 account fails with `ForeignAccountKey`; on an encrypted HD wallet, `add_account(Standard{5}, Some(own xpub))` while `Full` succeeds, with a foreign xpub is refused, and while `Locked` is `InvalidState`; on an encrypted `Seedless` wallet, `add_account(Standard{0}, Some(_))` is refused.
  - **Relabelled slots (D72):**
    - A `to_bytes` output whose `standard_bip44_accounts[0]` holds an account labelled `DashpayExternalAccount{..}` with a foreign xpub: `from_bytes` → `unlock` → `Tampered(AccountXpub(StandardBip44(0)))`, and the wallet stays `Locked`.
    - The same with `provider_operator_keys` labelled `DashpayExternalAccount` and a foreign operator key → `Tampered(SpecialAccountKey(ProviderOperatorKeys))`.
    - A BIP44 account whose stored `network` differs from the wallet's → `Tampered(AccountXpub(StandardBip44(0)))`.
    - `encrypt` of an unencrypted wallet with the first relabel → `ForeignAccountKey(StandardBip44(0))`.
    - After removing the named account through `wallet.accounts`, the encrypted wallet unlocks (D73). Re-adding it with `add_account(Standard{0}, None)` while `Full` gives back the original receive addresses (D87).
- The time a failure takes does not depend on the passphrase beyond the KDF itself (§5).

**Unlock timeouts and relocking.**
- These belong to the caller (WL2a, desktop), because key-wallet holds no clock (D11).
- WL1 provides the pure helper `pub fn relock_deadline(now_unix: u64, timeout_secs: i64) -> Result<u64, CryptoError>` with Core's rules: a negative timeout is an error (Core −8), and the timeout is clamped at `100_000_000` (`encrypt.cpp:59-66`).
- `WalletManager` stores nothing time-related.

#### 2.6.1 `upgradetohd` on an encrypted seedless wallet (Core `wallet/rpc/wallet.cpp:340-436`)

Core supports `createwallet blank=true passphrase=…` followed by `upgradetohd mnemonic mnemonicpassphrase walletpassphrase`. It checks HD-already, private-keys-disabled and (for an encrypted wallet) an empty wallet passphrase first, throwing before it touches the lock state (`:373-387`); only then does it lock, unlock with the wallet passphrase (`:391-405`), install the HD chain or descriptors under the encryption key (`:408-425`), and relock (`:427-430`). The mnemonic is validated only inside that install step (`SetupDescriptorScriptPubKeyMans` / `GenerateNewHDChain` run `CMnemonic::Check`, `wallet.cpp:4412-4414`, `hdchain.cpp:71-73`), so a bad phrase is reported after the passphrase check. WL1 specifies the same operation, with the same error order, and states its own end state: every failure after the unlock ends `Locked` (D47). possible security note: see manager.

```rust
impl Wallet {
    /// Core upgradetohd on an Encrypted wallet whose kind is Seedless.
    pub fn upgrade_to_hd_encrypted(
        &mut self,
        wallet_passphrase: &[u8],
        phrase: &str,                      // parsed at step 3, as Core checks it at install time (D47)
        compat: MnemonicCompat,            // DashCore on the node
        passphrase: Passphrase,
        options: WalletAccountCreationOptions,
        policy: &KdfPolicy,
    ) -> Result<(), UpgradeError>;
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum UpgradeError {
    #[error("wallet is already HD")] AlreadyHd,                          // Core -4 "Cannot upgrade a wallet to HD if it is already upgraded to HD"
    #[error("private keys are disabled for this wallet")] PrivateKeysDisabled, // Core -4
    #[error(transparent)] Crypto(#[from] CryptoError),                   // EmptyPassphrase, IncorrectPassphrase, Locked, …
    #[error("a seedless wallet with caller-supplied accounts cannot be upgraded")] CallerSuppliedAccounts, // D60
    #[error(transparent)] Mnemonic(#[from] MnemonicError),
}
```

Steps, in order:
1. **Preconditions, in Core's order (`:373-387`), before the lock state is touched.** Any wallet with an HD root (variants 0, 1, 2, 5, or `Encrypted` whose `kind` is not `Seedless`) → `AlreadyHd`. `WatchOnly`/`ExternalSignable` → `PrivateKeysDisabled`. An unencrypted `Seedless` wallet → `Crypto(NotEncrypted)` (use `upgrade_to_hd`). Empty `wallet_passphrase` → `Crypto(EmptyPassphrase)`; WL2b maps it to Core's −14 "Error: Wallet encrypted but supplied empty wallet passphrase", and a missing parameter to −13 before calling. Then, WL1's own check: any account whose origin is not `External` → `CallerSuppliedAccounts` (D60; Core's blank wallet has no such accounts). A precondition failure returns at once: the lock state is exactly what it was (`Unlocked` stays `Unlocked`, `UnlockedMixingOnly` stays so, `Unencrypted` stays `Unencrypted`) and the encoded bytes are unchanged.
2. **Relock, then unlock Full.** The in-memory MK and plaintext are dropped (`lock()`), then the MK is unwrapped with `wallet_passphrase` under `policy` (budget applies; no floor on reads, D44) and the unlock check runs (D43). A wrong passphrase → `Crypto(IncorrectPassphrase)`.
3. **Parse the phrase.** `WalletMnemonic::parse(phrase, compat)`; a failure → `Mnemonic(..)`. This is where Core's −1 from `CMnemonic::Check` arises, after the passphrase check, so a call with both a wrong passphrase and a bad phrase reports `IncorrectPassphrase`, as Core does.
4. **Build the new state off to the side.** Derive the seed (`WalletMnemonic::to_seed`), the root key and the accounts for `options`; build the new kind-1 plaintext `encoding ‖ records digest ‖ padding` in a zeroizing buffer (the `WalletType` 5 or 0 per D2, the digest of the unchanged kind-2 set, D61, and the padding from the D63 size pass); seal it as a new kind-1 `SealedRecord` under the **existing** MK with a fresh nonce. Nothing in `self` has changed yet.
5. **Commit in one step.** Replace the kind-1 record (variant 6 → a fresh-nonce variant 0 or 5 record, D33), `kind`, `root_xpub` and the `AccountCollection` together. Kind-2 records (imported private keys) are not touched and stay byte-identical; `wallet_id` is unchanged and `id_origin = Stored` (D10, D42). The MK record is unchanged, so the wallet passphrase is unchanged. The new kind-1 plaintext carries the unchanged records digest (D61).
6. **Relock.** The wallet ends `Locked`, as Core relocks (`:427-430`).

**Postconditions (D31, D32).**
- A call that fails a step-1 precondition leaves both the lock state and the encoded bytes (§2.8) as they were.
- A call that passes step 1 ends `Locked` on every outcome: success, wrong passphrase, KDF over budget, a bad phrase, or any failure in steps 4–5. On failure its encoded bytes are identical to the bytes before the call.
- **End state on failure (D47).** WL1 relocks after every failure past the unlock, a bad phrase included: a failed upgrade leaves no keys usable. The errors and their order are Core's.
- The unwrapped MK and every intermediate buffer are zeroized on all paths.

**The unencrypted case with a wallet passphrase.** Core encrypts after upgrading when the wallet was unencrypted and `walletpassphrase` is given (`:431-436`). The library composes `upgrade_to_hd` then `encrypt` (at `policy.write_params()`); WL2b sequences the two calls and, like Core, keeps the upgrade if encryption then fails (−16).

**Tests (WL1-6).**
- Encrypted seedless wallet with two imported private keys and one watch-only script → `upgrade_to_hd_encrypted` → `Locked`; ID unchanged; kind-2 record bytes unchanged; kind-1 record has a new nonce; after `unlock`, the imported keys still sign and the first 1,000 BIP44 external/internal addresses equal those of the same mnemonic and passphrase upgraded on an unencrypted wallet.
- **Precondition failures keep the lock state** (Core `:373-387`): an already-HD encrypted wallet, unlocked (Full) before the call → `AlreadyHd`, still `Unlocked`; the same wallet in `UnlockedMixingOnly` → still `UnlockedMixingOnly`; the same wallet `Locked` → still `Locked`; a `WatchOnly` wallet → `PrivateKeysDisabled`, still `Unencrypted`; an unencrypted `Seedless` wallet → `NotEncrypted`, still `Unencrypted`; an encrypted seedless wallet unlocked before a call with an empty wallet passphrase → `EmptyPassphrase`, still `Unlocked`. In every case the encoded bytes are unchanged.
- **Error order (D47):** an already-HD wallet with a bad phrase → `AlreadyHd` (not `Mnemonic`); an encrypted seedless wallet with a wrong passphrase and a bad phrase → `IncorrectPassphrase`; the same with the right passphrase and a bad phrase → `Mnemonic(Checksum)`, and the wallet is `Locked` even if it was `Unlocked` before the call (D47), bytes unchanged. The unencrypted `upgrade_to_hd` with a bad phrase on an already-HD wallet → `AlreadyHd`; on an encrypted wallet → `Crypto(EmptyPassphrase)`.
- **Failures after the preconditions end `Locked`** (Core `:391-405`): a wrong wallet passphrase on an encrypted seedless wallet that was `Unlocked` before the call → `IncorrectPassphrase`, now `Locked`, bytes unchanged; the same with a `KdfOverBudget` policy → `Locked`, bytes unchanged; a fault injected after step 3 (test hook) → `Locked`, bytes unchanged.
- **Round trip (D33):** blank → `encrypt` (kind-1 record holds variant 6) → `to_bytes`/`from_bytes` → `upgrade_to_hd_encrypted` → `to_bytes`/`from_bytes` → `unlock`: the kind-1 record now holds variant 0 or 5 with a new nonce, the kind-2 records are byte-identical, `wallet_id` is unchanged, and the addresses match the unencrypted upgrade.
- The transcribed `wallet_upgradetohd.py` and `wallet_createwallet.py` sequences: blank → encrypt → upgrade; blank → upgrade → encrypt.

**Existing paths on an `Encrypted` wallet.**
- **Private-key derivation** (`derive_private_key*`, `derive_extended_private_key`, FFI `wallet_derive_private_key*`): when locked, these return the existing `Error::InvalidState("wallet is locked")`, so no new `key_wallet::Error` variant is needed and the FFI is unchanged (D17).
- **Imported-key signing:** `imported_signing_key` needs `Full` and decrypts one kind-2 record on demand into a `Zeroizing` buffer (§2.5, D41).
- **Seed extraction:** the deprecated `wallet_seed_bytes` returns `None` on any `Encrypted` wallet (it has no error channel, §2.4.1); `seed_zeroizing` returns `Err(Locked)`/`Err(MixingOnly)` unless fully unlocked.
- **Public derivation** keeps working when locked: account xpubs live in `AccountCollection`, and `root_xpub` is public.

**`WalletManager` wrappers:** `encrypt_wallet`, `unlock_wallet` (returns `UnlockOutcome`; the caller persists the wallet when `kdf_upgraded`), `lock_wallet`, `lock_wallet_to_mixing_only`, `change_wallet_passphrase` and `lock_state(wallet_id)`. Their names and semantics follow Core's RPCs one to one. `build_and_sign_transaction` calls `require(SecretUse::General)`. A CoinJoin signing entry point (WL6) will call `require(SecretUse::CoinJoin)`.

### 2.7 Message signing (`message.rs`, always on)

```rust
pub const MESSAGE_MAGIC: &[u8] = b"DarkCoin Signed Message:\n";      // util/message.cpp:25

/// util/message.cpp:74-80. Arbitrary bytes (not &str): Core's message is a byte string.
pub fn message_hash(message: &[u8]) -> sha256d::Hash;

/// MessageSign (util/message.cpp:58-72) = CKey::SignCompact (key.cpp:254-275): RFC 6979, no extra entropy.
pub fn sign_message(key: &SecretKey, compressed: bool, message: &[u8]) -> String;   // base64, 88 chars
pub fn sign_message_compact(key: &SecretKey, compressed: bool, message: &[u8]) -> [u8; 65];

/// MessageVerify (util/message.cpp:27-56), same order, same results.
pub fn verify_message(address: &str, signature: &str, message: &[u8], network: Network) -> MessageVerificationResult;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageVerificationResult {
    InvalidAddress,      // ERR_INVALID_ADDRESS    → RPC -5 "Invalid address"
    AddressNoKey,        // ERR_ADDRESS_NO_KEY     → RPC -3 "Address does not refer to key"
    MalformedSignature,  // ERR_MALFORMED_SIGNATURE→ RPC -3 "Malformed base64 encoding"
    PubkeyNotRecovered,  // ERR_PUBKEY_NOT_RECOVERED → false
    NotSigned,           // ERR_NOT_SIGNED         → false
    Ok,                  // → true
}

/// Port of Core's DecodeBase64 (util/strencodings.cpp:139-172): length % 4, ≤ 2 '=', zero pad bits.
pub fn decode_base64_core(s: &str) -> Option<Vec<u8>>;

/// RecoverCompact (pubkey.cpp:199-217): any header byte; recid = (h-27)&3, compressed = (h-27)&4 in i32.
pub fn recover_compact(sig: &[u8], hash: &sha256d::Hash) -> Option<PublicKey>;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum SignMessageError {
    #[error("Invalid address")] InvalidAddress,                    // -5
    #[error("Address does not refer to key")] AddressNoKey,        // -3
    #[error("Private key not available")] PrivateKeyNotAvailable,  // -4 (SigningResult::PRIVATE_KEY_NOT_AVAILABLE)
    #[error("Sign failed")] SigningFailed,                         // -5
    #[error(transparent)] Locked(#[from] CryptoError),             // -13
}
```

Wallet integration, as Core's `CWallet::SignMessage` (`wallet.cpp:2405-2416`) does:

```rust
impl<T: WalletInfoInterface + …> WalletManager<T> {
    /// signmessage: find the key for `address` in the HD pools (via the managed info's address index)
    /// or in ImportedKeys; require(SecretUse::MessageSign); sign with the key's compression.
    pub fn sign_message(&self, wallet_id: &WalletId, address: &Address, message: &[u8]) -> Result<String, SignMessageError>;
}
```

Notes:
- **Address network.** `verify_message` parses the address and requires `network`, as Core's `DecodeDestination` does with the active chain parameters. An address for another network is `InvalidAddress`.
- **P2SH.** A P2SH address is `AddressNoKey`.
- **Compression.** HD keys are always compressed, as Core's are. Imported keys use the compression they were imported with.
- **The `dash` crate is not modified (D13).** The new module computes the hash from `MESSAGE_MAGIC` and `consensus_encode` of a `VarInt`. A test asserts it equals `dash::sign_message::signed_msg_hash` for UTF-8 inputs.

### 2.8 Persistence of the wallet (layout pin and envelope v1)

- **Legacy layout, pinned.** `Wallet`'s bincode encoding for wallets without WL1 features is unchanged, byte for byte. A WL1 feature means any of: `imported` non-empty, `WalletType` index ≥ 5, or `id_origin = Stored` (D42).
- **Pin tests, encode and decode gate.** WL1-1 commits the fixtures before the codec swap (the `SecretBytes` commit may sit between them):
  1. **Fixture commit (pre-swap).** With the existing `derive(Encode, Decode)` untouched, a generator test writes one fixture per pre-WL1 variant (`Mnemonic`, `Seed`, `ExtendedPrivKey`, `ExternalSignable`, `WatchOnly`; fixed seeds, fixed account set incl. BIP44, BIP32, CoinJoin, one key-only account, and `ProviderOperatorKeys` (BLS) and `ProviderPlatformKeys` (EdDSA) where the variant has a seed) to `key-wallet/tests/data/wallet_layout_v0/<variant>.bin`, plus the bincode-serde `<variant>.serde.bin` snapshots of §2.4 (D79). `AccountCollection` has `cfg(feature = "bls")`/`cfg(feature = "eddsa")` fields (`account_collection.rs:70-73`), so the layout depends on features; the generator therefore lives in key-wallet-ffi's tests and runs under its default set `bincode,bls,eddsa`, the bytes iOS and Android write (D53). key-wallet's own tests assert the same files byte-identical under the unified dev-dependency set (which adds `serde` and `test-utils`; neither changes the bincode layout, and the assertion proves it). key-wallet-manager's tests build key-wallet without `bls`/`eddsa` and do not decode these fixtures. The files are committed with sha256 pins, and the commit also adds the decode/encode assertions below, which pass against the derive.
  2. **Swap commit.** The hand-written codec replaces the derive. The same assertions must still pass, unchanged.
- **Assertions, for all five fixtures:** `new_encode(fixture_wallet) == golden_bytes`; `new_decode(golden_bytes) == fixture_wallet`, compared with a test-only `assert_wallet_eq` that checks `network`, `wallet_id`, the `WalletType` variant and every secret byte (through `expose()`/existing accessors), and the `AccountCollection` (its own derive is unchanged, so its re-encoding and `PartialEq` on `Account` both apply); `Wallet::restore(golden_bytes)` and `Wallet::from_bytes(golden_bytes)` give the same wallet; and, in key-wallet-ffi, the FFI path `wallet_manager_import_wallet_from_bytes` on the same bytes (through `WalletManager::import_wallet_from_bytes`, `key-wallet-manager/src/lib.rs:535-548`) returns the fixture's wallet ID, account count and first 20 receive addresses.
- **Implementation.** `Wallet` moves from `derive(Encode, Decode)` to a hand-written impl that produces identical bytes for the legacy case. That impl refuses (`EncodeError::Other`) to encode a wallet with WL1 features through the legacy entry points (`bincode::encode_to_vec(wallet)`, `Wallet::backup()`). A silent drop of `imported` is impossible.
- **Envelope v1.** `wallet/file.rs` defines it:
  ```
  0xFF ‖ b"KWW" ‖ version u8 = 1 ‖ flags u8 (bit0 = id_origin is Stored, written from Wallet::id_origin (D42); other bits must be 0)
       ‖ len u32 ‖ bincode(Wallet core fields incl. new WalletType variants)
       ‖ len u32 ‖ bincode(imported public entries)        (incl. each SealedPrivateKey entry's script, pubkey, label, birth)
       ‖ len u32 ‖ bincode(imported secrets)                 (unencrypted wallets; len = 0 when encrypted)
       ‖ count u32 ‖ count × (len u32 ‖ SealedRecord)       (encrypted wallets: exactly one kind-1 record, then one kind-2 record per SealedPrivateKey entry; count = 0 when unencrypted)
       ‖ end of input
  ```
  - All lengths are little-endian and checked against the remaining input before slicing; trailing bytes, an unknown flag bit, a record kind other than 1 or 2, or a second kind-1 record are `UnsupportedFormat` (D30). Kind-2 records pair with entries by the load rules of §2.5 (`Corrupt` otherwise, D41).
  - **Legacy branch (D59).** `from_bytes` on legacy bytes keeps today's behaviour of `import_wallet_from_bytes` (`lib.rs:540-545`), which decodes one `Wallet` and ignores trailing bytes. Making it strict could refuse files that load today; only the envelope is strict.
  - The leading `0xFF` is not a valid bincode varint start for the `Network` enum. Today's decoders therefore fail loudly on a v1 file instead of misreading it.
  - `Wallet::to_bytes()` writes legacy when possible and v1 otherwise. `Wallet::from_bytes()` reads both. `Wallet::backup()`/`restore()` keep their legacy-only behaviour and gain a doc pointer to the new pair.
  - `WalletManager::import_wallet_from_bytes` switches to `Wallet::from_bytes`. This is a superset, since legacy bytes decode as before. It keys the wallet by the origin-aware `compute_wallet_id()` (D42).
- **Encrypted wallets.** Their envelope contains no plaintext secret by construction (§5 scan test). Only the public metadata from §2.6 is in clear.
- **What WL2a stores.** WL2a's SQLite tables (`seed`, `keys`) store the `MasterKeyRecord` and `SealedRecord` bytes directly (A1 §2.20 persistence), and are free to store the envelope as a whole instead. The record formats in §2.6 are the stable contract.

### 2.9 Errors summary

- **New, `thiserror` 2, `#[non_exhaustive]`:** `MnemonicError`, `ImportError`, `CryptoError`, `UpgradeError` and `SignMessageError`. `ImportError` covers `PrivateKeysDisabled`, `NotSupportedForWalletType`, `InvalidKey` and `Locked(CryptoError)` (`CryptoError::Locked` or `CryptoError::MixingOnly`, D41). `CryptoError` gains `Tampered(TamperKind)` (whose account variants carry the `AccountSlot`, D73), `OutOfMemory` and `ForeignAccountKey(AccountSlot)` (D43, D45, D60, D72), and `UpgradeError` gains `CallerSuppliedAccounts` (D60); `TamperKind` is `#[non_exhaustive]` too.
- **Existing `key_wallet::Error` and `WalletError`:** no new variants (D17).
  - New `Wallet`-level functions that must return `key_wallet::Error` for consistency (constructors) map `MnemonicError` into the existing `Error::InvalidMnemonic(String)`. The string is `MnemonicError`'s `Display`, which never contains the phrase.
  - `WalletManager` passes the typed errors through new methods only.

### 2.10 FFI

**No existing exported function, struct, or enum value changes.** WL1 adds no new FFI functions. The required additions are append-only enum values plus mapping arms, all in one slice (WL1-5):

| Crate | Item | Addition |
|---|---|---|
| key-wallet-ffi | `FFIAccountKind` (`src/types.rs:204-239`) | `ImportedKeys = 16`, `ImportedWatchOnly = 17` |
| key-wallet-ffi | the 8 exhaustive matches (found by marking the enums `#[non_exhaustive]` in a scratch build and checking dependents, D50) | arms in `src/address_pool.rs:31` (`get_managed_account_by_type`) and `:96` (`…_mut`); `src/managed_account.rs:274` (`managed_wallet_get_account`), `:615` (`managed_core_account_get_account_type`), `:772` (`From<&AccountType>`) and `:1469` (`managed_core_account_get_address_pool`); `src/transaction_checking.rs:129`; `src/types.rs:329` (`from_account_type`) |
| key-wallet-ffi | address-pool getters for the imported kinds | `get_managed_account_by_type`/`…_mut` and `managed_core_account_get_address_pool` return null for `ImportedKeys`/`ImportedWatchOnly`: they have an `ImportedScriptPool`, not an `AddressPool`. The other arms map the kinds one to one |
| dash-spv-ffi | none directly | it uses `FFIAccountType::from` and compiles once key-wallet-ffi has the arms |

The public enums (`AccountType`, `ManagedAccountType`, `WalletType`, `AccountTypeToCheck`, `CoreAccountTypeMatch`) are **not** made `#[non_exhaustive]`: the crates are 0.x, appended variants are an accepted minor-version break, and downstream exhaustive matches (Platform's, §4.4) get a compile error that names every site, which is the point (D50).

**Consumer limits in WL1 (m9).** FFI callers keep exactly today's capabilities:
- `wallet_create_from_mnemonic*` (`key-wallet-ffi/src/wallet.rs:34-90`) and `wallet_manager_add_wallet_from_mnemonic*` (`key-wallet-ffi/src/wallet_manager.rs:127-233`) still parse strict BIP39 and derive with the empty passphrase. A mobile app that wants a passphrase must keep deriving the seed itself and use `wallet_create_from_seed*`, as the desktop does today.
- There is no FFI to encrypt, unlock, lock or change a passphrase, and no message-signing FFI.
- `wallet_manager_get_wallet` clones the manager's wallet (`key-wallet-ffi/src/wallet_manager.rs:387`), and clones of an encrypted wallet are locked (D56), so an FFI handle to an encrypted wallet is always `Locked`, even when the Rust side holds it unlocked (D69).
- A locked wallet surfaces as `FFIErrorCode::InvalidState`, which FFI callers cannot tell apart from other invalid states. With no lock event (D19), they would also have to poll for lock state. In practice no FFI caller can create an encrypted wallet in WL1, so this only matters for wallets handed in from Rust.

These limits are accepted for WL1 (the node wallet is a Rust consumer). **Follow-up WL1-F (D29)**, recommended as a BACKLOG row for the iOS/Android SDK owners and upstreamed with WL1: passphrase-aware constructors (`wallet_create_from_mnemonic_with_passphrase`, with a compat flag), `wallet_encrypt`/`wallet_unlock`/`wallet_lock`/`wallet_change_passphrase`/`wallet_lock_state`, a distinct `FFIErrorCode::WalletLocked` (appended value), and `message_sign`/`message_verify`. It is outside the node program's critical path.

---

## 3. Compatibility

### 3.1 Must match Core exactly (user-visible)

| Behaviour | Rule | Proof (§6) |
|---|---|---|
| Phrase acceptance (DashCore mode) | `core_check` ≡ `CMnemonic::Check` for every input | 513 Core-generated check cases + Core `bip39_vectors.json` + property tests against a transcribed C++ predicate |
| Mnemonic → seed with/without passphrase (DashCore) | `core_seed` ≡ `CMnemonic::ToSeed`, incl. NUL bytes, non-NFKD UTF-8, >248-byte passphrases | Core seed vectors + dashd `upgradetohd` vectors + regtest differential |
| Seed → keys/addresses | BIP32 from the 64-byte seed; BIP44 `m/44'/coin'/0'/{0,1}/i`, DIP9 CoinJoin `m/9'/coin'/4'/0'/0/i` | first 1,000 external + internal addresses per corpus case vs dashd (`listdescriptors`/`getnewaddress`) |
| Entropy → phrase, generation sizes | `FromData`; `-mnemonicbits` set {128,160,192,224,256} | Core vectors; `wallet_mnemonicbits` assertions |
| Message hash and signature bytes | RFC 6979, header `27+recid+4c`, base64 | `util_tests` `message_sign` vector byte-equal; 41 regtest dashd sign cases byte-equal |
| Verification verdicts | `MessageVerify` order and results, `RecoverCompact` header arithmetic, `DecodeBase64` strictness | `util_tests` `message_verify` (6 cases) + 11 regtest verify cases + generated cases with every header byte 0–255 |
| Imported key → script | compressed flag preserved, P2PKH; P2PK(pubkey) also owned, spendable iff the key is held (D48) | `importprivkey` of compressed and uncompressed WIF vs dashd `getaddressinfo`; P2PK-funded case in differential 3 |
| Lock-state transitions | Core's state machine incl. mixing-only rules (Full + any passphrase → Ok; MixingOnly → MixingOnly re-verifies; `Lock(fAllowMixing)`; `ChangeWalletPassphrase` end states, D46), -13/-14/-15/-16/-17 mapping | unit state-machine tests transcribed from `wallet_encryption.py`, `wallet_createwallet.py`, plus the per-case tests of §2.6 |
| `upgradetohd` error order | AlreadyHd → PrivateKeysDisabled → empty passphrase → wrong passphrase → bad phrase (D47) | §2.6.1 error-order tests |

In Bip39 mode, a strict phrase with an ASCII passphrase of 248 bytes or fewer derives the same seed as DashCore mode, so both agree with dashd in that case (`Portability::seeds_agree`).

### 3.2 Deliberately different

| Area | Ours | Core | Why |
|---|---|---|---|
| Encryption at rest | argon2id KEK, XChaCha20-Poly1305 MK wrap and records, AAD-bound | `BytesToKeySHA512AES` + AES-256-CBC, IV from pubkey hash/chain id | DEC-11: our file is not Core-compatible anyway; A1 §2.20 item 3 rejects re-implementing Core's crypter for writing. Strictly stronger KDF and authenticated encryption |
| KDF cost | `KdfPolicy::default()` (mobile): ≥ 64 MiB, t ≥ 3, ≥ 500 ms; `KdfPolicy::desktop()` (desktop, `dashd-wallet`): ≥ 256 MiB, t ≥ 3, ≥ 500 ms. A desktop-written file returns `KdfOverBudget` on a default reader until the caller opts in (D28). The floor binds writes only: a mobile-written file opens under `desktop()` and is re-wrapped at 256 MiB on unlock (D44) | ≥ 25,000 SHA-512 rounds, ~100 ms | FR-WAL-04 "at least as strong" |
| Wallet file | own versioned format (§2.8) | BDB/SQLite `wallet.dat` | DEC-11 |
| Strict-BIP39 default in the library | `MnemonicCompat::Bip39` | n/a | keeps iOS/Android/Platform behaviour; the node opts into DashCore (D3) |
| Non-English mnemonics | supported in Bip39 mode | English only | key-wallet already supports them; DashCore mode refuses non-English (Core would) |
| Tamper check on unlock | self-derived public metadata re-derived and compared; mismatch → `Tampered`, stays locked (D43, D60) | `CheckDecryptionKey` checks one key per SPK manager | Core's file is not authenticated either; the check is cheap next to the KDF |

### 3.3 Interaction with Core wallet import (WL3) and IM1

- **WL3 (`dash-coreimport::wallet`) is the consumer of every WL1 type.** Core's crypter is implemented decrypt-only there (A1 §2.20 item 3, ported from `dw-compat::crypter`). The import then re-seals into the §2.6 format under the user's passphrase, and plaintext lives only in zeroizing buffers in between. The record mapping is:
  - `hdchain`/`chdchain` → `WalletType::MnemonicWithPassphrase { mnemonic: WalletMnemonic::DashCore, … }`, or `Seed` when no mnemonic is stored. The counters go to WL2a.
  - `walletdescriptorkey`/`walletdescriptorckey` with Dash's mnemonic fields → the same.
  - loose `key`/`ckey` → `ImportedKeys` (spendable), so a legacy non-HD wallet becomes `Seedless` + imported keys.
  - `watchs` / watch-only scripts → `ImportedWatchOnly`.
- **Dual derivation.** A1 §2.20 item 1 says "accept both forms, prefer the one that derives known keys". Concretely: when `Portability::seeds_agree()` is false for a stored phrase and passphrase, WL3 derives with both modes and keeps the one whose first keys match the file's pubkeys. Core-written wallets always match DashCore. A desktop-written phrase may match Bip39.
- **IM1 (net-state files) is unaffected.** IM1a/IM1b import no wallet data. WL1 gives IM1 nothing and needs nothing from it. Only the "untrusted file → secret" boundary is shared, so both are `+SEC`.
- **Exports back to dashd (IM2, US-IMP-09).** A mnemonic plus passphrase exported from our wallet is accepted by dashd's `upgradetohd`/`-mnemonic` when `Portability::core_accepts`. A Bip39-mode wallet whose seeds do not agree is flagged by IM2's export report.

---

## 4. Consumers

### 4.1 dash-spv and dash-spv-ffi

- **One additive change in dash-spv (D49).** It is generic over `WalletInterface`, and no existing trait method signature changes. The imported scripts arrive through the existing `monitored_script_pubkeys_for` (block filters) and `monitored_addresses` (mempool), and the earlier birth through `earliest_required_height`, which already has a default.
  - A raw script with no address form (bare multisig, any `importaddress <hex>` script, and the P2PK aliases of §2.5) cannot be expressed as an `Address`, so it never reaches the mempool bloom filter that `load_bloom_filter` builds from `monitored_addresses()` and `watched_outpoints()` (`dash-spv/src/sync/mempool/manager.rs:140-155`). WL1 adds a required `WalletInterface::monitored_extra_scripts() -> Vec<ScriptBuf>`, aggregated from the required per-wallet `WalletInfoInterface::monitored_extra_scripts()` (D78). The dash-spv side has three parts (D64):
    - **Guard.** `load_bloom_filter` returns early today when `addresses.is_empty() && outpoints.is_empty()` (`manager.rs:152-155`); the guard also requires the extra-script push list to be empty, so a seedless wallet whose only imports are raw scripts or P2PK keys loads a filter.
    - **Filter input and size.** `build_wallet_bloom_filter(addresses, outpoints, extra_pushes, …)` (`filter.rs:33-48`) gains the push list, inserts every data push of each extra script, and sizes the filter from `addresses.len() + outpoints.len() + extra_pushes.len()`.
    - **Rebuild.** Rebuilds follow `monitor_revision()` (`sync_manager.rs:133-138`), which now changes on every import (§2.5), so an import into a running client rebuilds the filter.
    The filter uses `BloomFlags::All` (`filter.rs:41, 47`), so spends of matched outputs are followed as today.
  - **Residual gap.** A script with no data push (for example `OP_TRUE`) cannot be matched by a bloom filter; it is seen at confirmation through the block filters only, where dashd sees it in the mempool. This is documented in the API docs of `monitored_extra_scripts`.
  - Tests (WL1-5): a regtest wallet with an imported bare 1-of-2 multisig script sees an unconfirmed payment to it in the mempool; a P2PK payment to an imported key likewise; a `Seedless` wallet whose only import is a raw script loads a filter and sees the payment; the filter's element count equals addresses + outpoints + extra pushes (unit test on `build_wallet_bloom_filter`); **import while running:** the client is synced and idle, the raw script is imported, `monitor_revision` changes, the filter is rebuilt, and a payment broadcast afterwards is seen unconfirmed.
- **History scan after an import (D65, D77, D81).** Once a wallet has committed progress, `scan_start = max(birth, committed + 1)` (`filters/manager.rs:212-240`), so lowering `earliest_required_height` alone does nothing after the first sync. The import wrappers therefore rewind the wallet's scan cursor through the #649 path, and `earliest_required_height` includes the imported births so the restarted scan is not floored above them (§2.5, D81).
  - **dash-spv needs no source change for any birth at or above the header anchor.**
    - A rewound wallet is behind: `FiltersManager::tick` detects it (`wallets_behind`), calls `reset_for_rescan()` and then `start_download()` (`filters/sync_manager.rs:213-247`).
    - `start_download` (`filters/manager.rs:189`) contains the clear-and-re-download branch (`:265-283`).
    - Filter headers are stored from the anchor (`filter_headers/manager.rs:153-159`), so every filter at or above it can be fetched again while the client runs.
  - **Tests (WL1-5, regtest).**
    - **In range.** Sync a wallet, pay an address of a fresh key at height h, and mine past it. Import the key with `Height(h − 1)` while a scan batch is in flight. The old payment is found, and the balance includes it.
    - **Below the stored filter start (D77).** Use a client anchored below h whose stored filters start above h, and import with a birth below the first stored filter but at or above the anchor. The old payment is found without a restart.
    - If either test shows that the existing path needs more than a small change, WL1-5 keeps only the key-wallet signal, and the row below takes the rest.
  - **Below the anchor.** A checkpoint-synced client has no headers below its anchor, so history there is never scanned. `start_download` logs the case (`filters/manager.rs:246-255`), and the import wrappers' API docs state it (§2.5). On mainnet a fresh client's anchor is at least the HD activation height (`lifecycle.rs:113-120`), so by default this covers every import born before it (D95).
    - **Backlog row (for the manager to file):** "dash-spv: scan history below the header anchor of a checkpoint-synced client when a wallet's birth or an import's birth lies below it (needs header backfill below the anchor). Owner: dash-spv sync (SPV track)."
- **No new `WalletEvent` variants**, because `dash-spv-ffi/src/callbacks.rs` matches `WalletEvent` exhaustively (D19). Lock-state notifications are WL2a's own channel. Transaction events for imported accounts reuse the existing variants with the new `AccountType`s.
- **Dependency graph unchanged.** `encryption` is off by default in key-wallet, and key-wallet-manager forwards it as an opt-in feature. dash-spv does not enable it, so its dependency graph, compile time and the PF0 "dash-spv wallet benches" guard are unaffected.
  - The always-on additions (`secret`, `mnemonic/dash_core`, `message`, `imported`) need no new crate except `thiserror` 2, which is already in the lockfile.
- **Cost of imported entries.** An extra branch per output in routing is the only steady-state cost when no imports exist. It is guarded by BG `crit:` `filter_scan` and `address_generation`, plus a new `message`/`mnemonic` micro-bench that is not on the guard list.
- **Rescans.** An SPV wallet with an `ImportBirth::Genesis` entry is rewound to 0 and its `earliest_required_height` becomes 0 (D65, D81), so dash-spv re-scans filters from height 0 for that wallet once, at import time. This is correct and expensive. The desktop is expected to supply a birth height (WIF sweep UI), and the risk is listed in §9. Because imported births now enter `earliest_required_height` (D81), they also lower a fresh client's start height (`lifecycle.rs:117`): a `Genesis` import on testnet syncs headers from 0 (D95).

### 4.2 Node wallet (`dashd-wallet`, WL2a/WL2b/WL3/WL6)

| Core surface | WL1 API used |
|---|---|
| `-mnemonic`, `-mnemonicpassphrase`, `-mnemonicbits`, `upgradetohd`, `createwallet` | `DashCoreMnemonic::{parse, generate}`, `Wallet::from_mnemonic_with_passphrase` (always `MnemonicCompat::DashCore`, D3), `Wallet::upgrade_to_hd`, `Wallet::upgrade_to_hd_encrypted` (§2.6.1), `CORE_LEGACY_MAX_PASSPHRASE_BYTES` on the legacy-kind path |
| `createwallet blank=true` / `disable_private_keys=true` | `Wallet::new_seedless` / `Wallet::new_watch_only` |
| `dumphdinfo`, `listdescriptors true` | `Wallet::mnemonic_passphrase`, `WalletMnemonic` phrase, `require(SecretUse::Export)` |
| `importprivkey`, `importpubkey`, `importaddress`, `importmulti` | `Wallet::import_*` via `WalletManager` (gates of §2.5); kind gates (descriptor vs legacy) in WL2b |
| `encryptwallet`, `walletpassphrase`, `walletlock`, `walletpassphrasechange` | `Wallet::{encrypt, unlock, lock, lock_to_mixing_only, change_passphrase}`, `UnlockOutcome` (persist on `kdf_upgraded`), `relock_deadline`, `CryptoError` → RPC codes |
| `signmessage`, `signmessagewithprivkey`, `verifymessage` | `WalletManager::sign_message`, `message::{sign_message, verify_message}` |
| CoinJoin under `mixingonly` (WL6) | `require(SecretUse::CoinJoin)` |

- **WL2a requirement: managed pools after unlock (D75).** The unlock check authenticates the `Wallet` envelope only. `ManagedWalletInfo`'s address pools are persisted separately by WL2a and are not covered.
  - After every unlock, and at load, WL2a rebuilds the cached pool scripts from the authenticated account xpubs, or verifies them against those xpubs, before it hands out any receive or change address.
  - A mismatch is treated like `Tampered`: the pool is rebuilt and the event logged.
- **WL2a requirement: import persistence order (D81).** For every import, WL2a persists the wallet's lowered scan cursor first and the new imported entry second, both before the RPC returns, holding the wallet lock across the import and both writes (D94). This is the only ordering contract between the wallet file and the managed info. A crash between the writes costs one harmless rescan, and the user retries the import (§2.5).
- **WL2a requirement: imported funds after load (D82).** The imported managed accounts are rebuilt empty at load. WL2a replays its `txs` records in block order, each with its stored context, through the transaction-checking path after the rebuild, then calls `update_balance`. All of it completes before any block or mempool transaction is processed and before any build or RPC is served (D91). Imported balances return without a rescan, as in Core.

### 4.3 Desktop (dashwallet-desktop)

- **What it uses today.** The desktop depends on upstream `dashpay/rust-dashcore` at `8fe0a381…`, patched to the PastaPastaPasta fork `d3d50520a` for #1150 (`rust/Cargo.toml:50-54, 87-89`), and drives key-wallet through `platform-wallet`.
- **It already implements WL1's features for itself:**
  - `dw-compat::bip39core` (weak checksum, no NFKD, salt cut at 256);
  - `dw-vault`: argon2id with floor m = 256 MiB, t = 3, p = 1 and t calibrated to ≥ 500 ms, XChaCha20-Poly1305, AAD-bound records, mixing-only unlock;
  - `dw-message` (Core message signing).
- **How it passes passphrases today.** It derives the seed itself and hands key-wallet only the 64 bytes (`dw-engine/src/keys.rs:573`, `create_wallet_from_seed_bytes`). This is why key-wallet's hard-coded empty passphrase never bit it.
- **What WL1 changes for the desktop:** nothing until upstreaming (D21). `node/integration` is never pushed (DEC-09), and nothing from this program may enter the desktop repo except through the upstream crates.
- **After upstreaming**, the desktop can do three things:
  - replace `dw-compat::bip39core` with `key_wallet::mnemonic::dash_core`;
  - replace `dw-message` with `key_wallet::message`;
  - register imported WIF keys as `ImportedKeys` instead of refusing them (`dw-engine/src/compat.rs:57, 68`), which unblocks US-WAL-15 D.
- **`dw-vault` stays.** Its OS-store and biometric slots, grants and DIP-15 scope are app policy, so keeping it is correct (FR-DSK-02). WL1's §2.6 format is deliberately the same shape: 2 levels, the same KDF and AEAD, the same AAD discipline. A later desktop decision could adopt `SealedRecord` for interchange.
- **Vectors.** The desktop's Core-generated vector files are reused verbatim with provenance (§6.1). It is pasta's own public repository (DEC-13), and the files are produced by Core and dashd.

### 4.4 Upstream consumers outside this repository (Platform, iOS, Android)

- **Platform (`rs-platform-wallet-ffi`)** matches `AccountType` exhaustively, for example `persistence.rs:4179`, `:4793`, `:6667`, `core_wallet_types.rs:1073` and `manager_diagnostics.rs:925` (platform `6499c680c6`), and persists account-type tags. When it upgrades to a key-wallet release containing WL1-5, those matches fail to compile until they gain arms for `ImportedKeys`/`ImportedWatchOnly`, and its tag table needs two new values. This is the accepted cost of appending variants under 0.x (D50); the enums stay exhaustive so that the compiler lists every site.
- **iOS/Android SDKs** see only key-wallet-ffi: the appended `FFIAccountKind` values (§2.10). Existing persisted values are unchanged.
- **Nothing else changes for them in WL1.** Files they write stay in the legacy layout (no WL1 features are reachable through the FFI), and the goldens of §2.8 pin that layout under their feature set.
- **Required trait methods (D78, D81).** WL1-5 adds three required methods, with no defaults, to `WalletInfoInterface` (`wallet_info_interface.rs:49`):
  - `monitored_extra_scripts(&self) -> Vec<ScriptBuf>`;
  - `sync_imported_pools(&mut self, imported: &ImportedKeys)`;
  - `rewind_scan_cursor(&mut self, height: CoreBlockHeight)`.

  It also adds one required method to `WalletInterface` (`key-wallet-manager/src/wallet_interface.rs:62`): `monitored_extra_scripts`.
  - **Who implements them.** In this repository, `ManagedWalletInfo` implements the three. `WalletManager<T>` (`process_block.rs:19`) and the three mocks (`test_utils/mock_wallet.rs:133, 301, 452`) implement the `WalletInterface` one.
  - **Platform's cost.** `PlatformWalletInfo` (`rs-platform-wallet` `platform_wallet_traits.rs:33`) needs three delegation arms to `self.core_wallet` when it upgrades. That cost sits next to the D50 match arms. No Platform persistence schema changes, because WL1 adds no persisted `ManagedWalletInfo` field (D83).
  - **Why not defaults.** A defaulted no-op would make `WalletManager<PlatformWalletInfo>` imports silently skip both the rescan and the mempool filter. A required method makes the compiler list the site.
- **Build entry points (D84).** `build_and_sign_transaction`, `build_and_sign_transaction_with_signer` and `build_unsigned_transaction` gain a trailing `change_address: Option<Address>`. No Platform package calls them. The callers, all passing `None`, are key-wallet-ffi `transaction.rs:138`, key-wallet-manager `lib.rs:787`, `dash-spv/tests/dashd_sync/tests_transaction.rs:702` and 13 key-wallet unit tests (`transaction_building.rs:753-1273`). Platform's cost is none. This is a 0.x source break like D50 (D92).
- **Platform's own change selection (D92).** `rs-platform-wallet` builds through `TransactionBuilder::add_funding` directly (`wallet/core/transaction.rs:695-697`, `broadcast.rs:482`) and picks change in `next_change_address_for_account` (`wallet.rs:271-307`), so it chooses change without `fund`. Before Platform holds a `Seedless` wallet, those paths must set an explicit change address or refuse.
- **DashPay contact keys (outside WL1, D74).** A contact's xpub (`DashpayExternalAccount`) is unauthenticated in the wallet file (§5.1), and payments to the contact are derived from it. Platform should re-check it against the contact request before paying.
- The upstream PR description for WL1-5 carries these notes so that the Platform maintainers can plan the arms.

---

## 5. Security

### 5.1 Threat model

| Adversary | Capability | Protection |
|---|---|---|
| Disk thief / backup leak | reads wallet files, backups, SQLite pages, swap | encrypted wallets: §2.6, no plaintext secret in any file (scan test); argon2id at ≥ 64 MiB / t ≥ 3 under `default()` (mobile) and ≥ 256 MiB / t ≥ 3 under `desktop()` makes offline guessing costly |
| File tamperer | edits KDF params, swaps records between wallets, truncates, appends unknown records, edits public metadata | AAD binds params, salt, wallet id, kind, record id and version, and the nonce is authenticated as the AEAD nonce; the kind-1 plaintext carries a digest of the count and ids of the kind-2 records (D43, D61); unknown versions, kinds and trailing data refused (D30). **Unauthenticated while locked (Core parity):** `root_xpub`, the account xpubs, `network`, imported public entries (watch-only scripts, pubkeys, labels, births) and anything derived from them, so a tampered file can show wrong addresses and balances until unlocked. **At unlock**, the root, every self-derived account and every imported key are re-derived and compared; a mismatch is `Tampered` and the wallet stays locked (D43). **Never authenticated:** watch-only entries and labels; `External` accounts (DashPay contacts' xpubs) and `CallerSupplied` accounts (every account of a `Seedless` wallet, and BLS/EdDSA accounts of an `ExtendedPrivKey` wallet), which the wallet cannot derive (D60). What a tampered one does:
  - a tampered `External` xpub is where payments **to that contact** are derived, so later payments to the contact go to the tamperer;
  - a tampered `CallerSupplied` account redirects its receive addresses, and would redirect change taken from it, so seedless spends take an explicit change address and never use a `CallerSupplied` account for change; the guard sits where change is chosen (D74, D84).

  **API boundary.** The add gates keep a foreign key out of a spending slot of an encrypted wallet only through `add_account`, `add_bls_account` and `add_eddsa_account` (D60). Writing `Wallet.accounts` directly is outside the guarantee. The slot that `Tampered` names lets the caller remove such an account (D73).

  **Slot consistency.** An account whose stored type or network disagrees with its slot fails the unlock check (D72).

  **Managed pools (D75).** The check covers the `Wallet` envelope only. `ManagedWalletInfo`'s cached pool scripts are persisted separately by the consumer and are not covered, so WL2a rebuilds or verifies them from the authenticated xpubs after unlock, before it hands out any address (§4.2). Lowered KDF parameters cannot unwrap the MK (they are in the AAD) |
| Crafted-file sender | hands a wallet file with expensive KDF parameters to a reader | the pre-authentication budget (D28) bounds the work before argon2 runs, with an explicit caller override up to a fixed ceiling. It bounds the cost; it does not remove it |
| Local log/telemetry reader | reads tracing output, panic messages, error strings | WL1's new secret types implement no `Display` and redact `Debug`; `Mnemonic` and `RootExtendedPrivKey` get redacted `Debug` (D40); the existing `Display` impls of `Mnemonic` (phrase), `ExtendedPrivKey` (xprv) and `Seed` (8 bytes) are unchanged and outside the claim (m13); errors carry positions, counts and account slots by kind and number only (D86); `tracing` never given secret fields (lint test) |
| Process-memory reader after lock | core dump, later heap reuse | MK and plaintext `WalletType` zeroized explicitly on `lock()` and `Drop`, and `WalletType` gains `Drop` (D45); imported keys of an encrypted wallet exist only sealed (D41); clones of an unlocked wallet come out locked (D56); argon2 blocks and the `Mnemonic` codec's strings wiped (D45); KDF and AEAD buffers `Zeroizing`; no `Copy` secret types introduced |
| Holder of an old file copy + old passphrase | a backup made before `change_passphrase`, and the passphrase it had | none: `change_passphrase` re-wraps the same MK, as Core's `ChangeWalletPassphrase` does, so the old `MasterKeyRecord` and old passphrase still yield the MK, which also opens records sealed after the change. A user whose passphrase leaked must create a new wallet and move funds; a `rekey()` is out of WL1's scope (D55) |
| Network peer (message verify) | sends arbitrary address/signature/message | total functions (no panics) over all inputs; fuzz target |
| Same-user process | — | out of scope (OS boundary), as for Core |

**PLAN §7.** WL1 touches no consensus code and has no §7 interaction that this designer knows of. No §7 marker is needed.

### 5.2 G-SEC review checklist (WL1 slices)

1. **Secrets never logged.**
   - `SecretBytes`, `DashCoreMnemonic`, `WalletMnemonic`, `ImportedKeys`/`ImportedEntry`/`ImportedKind` (manual redacted `Debug`, D35), `EncryptedWallet::unlocked`, `MasterKeyRecord` (wrapped) and every KDF/KEK/MK buffer have a redacted or absent `Debug` and no `Display`. `Mnemonic` and `RootExtendedPrivKey` get a redacted `Debug` (D40). The claim covers these types; the existing `Display` impls of `Mnemonic`, `ExtendedPrivKey` and `Seed` are not changed by WL1 (m13).
   - Test: the `{:?}` scan of §2.3 (chain code counted as secret).
   - Test: format every such value with `{:?}` and assert that no secret byte sequence (hex or ASCII) appears.
   - Test: a `tracing` capture layer over the whole WL1 test suite asserts that no known secret from the test vectors appears in any event.
2. **Errors carry no secret.** `MnemonicError`, `CryptoError`, `ImportError` and `SignMessageError` contain no input bytes. A review grep finds no `format!` with a phrase, passphrase or key in error paths.
3. **Constant-time comparisons.**
   - Secret equality goes through `subtle::ConstantTimeEq`: `SecretBytes`'s `PartialEq`, and any key or passphrase comparison.
   - Passphrases are never compared directly. Correctness is decided by the AEAD tag, which is constant-time in RustCrypto.
   - The `core_check` checksum compare works on public data and needs no constant time.
   - The mnemonic word lookup is constant-time only in the Core-mode `lookup()` behind `DashCoreMnemonic::check`; other mnemonic paths make no constant-time claim. The full constant-time path is WL1-7 work.
4. **KDF cost defaults.**
   - `KdfPolicy::default()` writes at `MOBILE_FLOOR` and `KdfPolicy::desktop()` at `DESKTOP_FLOOR`, each plus calibration (§2.6); the `TEST` params cannot be reached outside `cfg(test)` / the unforwarded `insecure-test-kdf` feature (D58).
   - Reading refuses parameters over the caller's budget (`KdfOverBudget`), over the fixed ceiling (`KdfAboveCeiling`) or under argon2's own minimums (`UnsupportedFormat`), all before argon2 runs (D28). There is no read floor (D44).
   - A unit test asserts that every write path (`encrypt`, `change_passphrase`, migrate-on-unlock) enforces the floor, and that unlock upgrades a below-floor file (D44).
5. **Memory zeroization.**
   - **Scope.** This item covers WL1's new types, the zeroizing siblings of §2.4.1 and the code paths WL1 adds or migrates. The deprecated accessors (`Mnemonic::to_seed`, `Mnemonic::phrase`, `Wallet::wallet_seed_bytes`) and `Seed` (`Copy`) keep their existing behaviour and are outside the claim (§2.4.1).
   - Every new secret type is `Zeroize + ZeroizeOnDrop`, and none is `Copy`. The imported containers are the exception by design: manual `Zeroize` over their secret bytes plus `Drop` on `ImportedKeys`, with the secret held in `Zeroizing` (D35).
   - Every seed WL1 code derives goes through `Zeroizing<[u8; 64]>`; no WL1 path calls a deprecated accessor (a clippy `disallowed-methods`/deprecation check enforces this in key-wallet).
   - `lock()` and `Drop for EncryptedWallet` call `mk.zeroize()` and `plain.zeroize()` explicitly before dropping them. (rev3 said the boxed `WalletType`'s "existing `Drop`" did this; `WalletType` has no `Drop` today, so WL1-1 adds one, D45.)
   - argon2's `zeroize` feature is on, and WL1 owns and wipes the block buffer (`hash_password_into_with_memory`, D45).
   - The `Mnemonic` bincode codec encodes from `phrase_zeroizing()` and decodes into `Zeroizing<String>` (D45).
   - Imported private keys of an encrypted wallet are held only sealed; signing decrypts one on demand into `Zeroizing` (D41).
   - Clones of an unlocked `EncryptedWallet` are locked (D56).
   - The intermediate bincode buffer of a sealed `WalletType` is `Zeroizing<Vec<u8>>`.
   - In the WL1 PBKDF2 (`pbkdf2_hmac_sha512_one_block`), the HMAC key and the pads it owns are wiped. The internal state of the hash engines (the per-round engine clones and their buffers) is not zeroized; WL1-7 covers it.
   - The review checks that no `.clone()` of a secret escapes into a non-zeroizing container. Residual risk from register spills, secp256k1 internals and allocator copies on `Vec` growth is documented, not claimed fixed (as `dw-vault/src/lib.rs:33-45` does). Buffers are pre-sized to avoid reallocation.
6. **Randomness.** Salts, nonces and the MK come from `getrandom`, and an RNG error is `CryptoError::Rng`, never a fallback. Without the `getrandom` feature, encryption is unavailable (compile error, not runtime).
7. **AEAD use.**
   - Nonces are 24 bytes and random, one per seal.
   - The AAD covers every header byte before the nonce, plus the wallet ID; the nonce itself is authenticated as the AEAD nonce input (§2.6). The tamper matrix mutates every byte offset of both records, nonce included.
   - On tag failure, no partial plaintext is exposed: decrypt into a buffer that is zeroized on error.
8. **Format parsing.** Lengths are checked before slicing; the ceiling, the pre-authentication budget and argon2's minimums are checked before argon2 allocates anything (D28, D44), and the allocation itself is fallible (`OutOfMemory`, D45); unknown versions, kinds, flag bits and trailing bytes are refused (D30). The fuzz targets cover `MasterKeyRecord::decode`, `SealedRecord::decode`, envelope v1 decode, `verify_message` and `DashCoreMnemonic::parse`.
9. **No plaintext at rest by the library.**
   - Encrypted wallets: a scan of `Wallet::to_bytes()` for every secret in the test corpus (mnemonic, passphrase, seed, root xprv bytes, WIF bytes) finds none, after every step of the US-WAL-06 sequence, and after an import while unlocked (D41).
   - The same scan over the in-memory `Wallet` (its `{:?}` and the bytes of every `imported` entry) after `lock()` finds none (D41).
   - Unencrypted wallets: plaintext only by explicit user choice (D14).
10. **API misuse resistance.**
    - `expose()` is the single accessor for secret bytes.
    - The plaintext legacy `backup()` documents its risk.
    - Node crates are expected to deny it via clippy `disallowed-methods` (WL2a acceptance item, recorded here).

---

## 6. Tests

### 6.1 Vectors (L0)

| Source | Content | Location after WL1 | Slice |
|---|---|---|---|
| Core `src/test/data/bip39_vectors.json` | 24 Trezor vectors: entropy → phrase, `Check`, `ToSeed("TREZOR")`, master xprv (`wallet/test/bip39_tests.cpp:22-62`) | already vendored: `test-vectors/core/0364ed626ec2/src/test/data/bip39_vectors.json`, loader `load_bip39` | WL1-2 |
| Core `test/util_tests.cpp` `message_sign`/`message_verify`/`message_hash` (develop `:1725-1828`; rc.3 `:1724-1827`) | 1 sign vector, 6 verify verdicts, hash construction | new `test-vectors/core/0364ed626ec2/transcribed/message_tests.json` + `PROVENANCE.md` row + `SHA256SUMS` | WL1-3 |
| Core `test/functional/rpc_signmessagewithprivkey.py:31-37` | regtest WIF, message, expected signature, P2PKH address | same transcribed file (`functional` section) | WL1-3 |
| Core `test/functional/wallet_upgradetohd.py:234-238, 258-267` | phrase with and without passphrase → first address; NUL-byte passphrase | new `transcribed/bip39_functional.json` | WL1-2 |
| dashwallet-desktop `testdata/bip39_core_quirks.json` (generated by Core `bip39.cpp` compiled from develop + regtest dashd v24.0.0 `upgradetohd`) | 513 `Check` cases with `core_check`/`strict_check`, 12 seed cases (empty, NUL, non-NFKD, 248/249/256-byte passphrases), 24 Trezor, dashd legacy seed + descriptor xprv cases | new `key-wallet/tests/data/core_bip39_quirks.json` (verbatim, provenance header: desktop commit `71c814a7`, generator paths) | WL1-2 |
| dashwallet-desktop `testdata/message_cases.json` (regtest dashd v24.0.0 `signmessagewithprivkey`/`signmessage`/`verifymessage`) | 41 sign, 11 verify, plus Core unit cases | new `key-wallet/tests/data/core_message_cases.json` (verbatim, provenance) | WL1-3 |

All new JSON gets a regeneration script, or a pointer to the desktop's `testdata/oracle/{bip39,dashd}` generators, and sha256 pins.

### 6.2 Differential checks

- **Oracle.** The pinned oracle (`dash-oracle-sys/pins.txt`: v24.0 `d4c16888b4`) exposes no BIP39, `MessageHash`, `RecoverCompact` or wallet entry point. Its only adjacent function is `pubkey_verify` (`dash-oracle-sys/src/lib.rs:1043-1044`).
  - WL1 uses `pubkey_verify` to cross-check that the `r‖s` part of every signature `sign_message_compact` produces verifies under Core's `CPubKey::Verify`. This runs over the 41 cases plus 1,000 random keys and messages, with the DER built from compact.
  - Extending the oracle with wallet functions is rejected (D15): wallet code is not consensus, and the oracle's scope is A1 §4.4.
- **Live regtest dashd.** The comparison runs through `dash-testkit` / `dash-spv::test_utils::DashCoreNode`, with dashd from `contrib/setup-dashd.py`. It is a `#[ignore]`-by-default test group, run by the gate like the dashd suites. It needs no new CI infrastructure.
  1. **Mnemonic corpus.** 200 phrases: Core-generated, plus weak-checksum phrases taken from the 513 `core_check && !strict_check` cases. They are combined with 12 passphrase shapes (empty, ASCII, NUL, NFC vs NFKD non-ASCII, 248/249/256/300 bytes).
     - For each pair, on a descriptor wallet: `createwallet blank` then `upgradetohd`. `listdescriptors` (public form, checksummed, including the inactive DIP9 descriptor) gives three descriptors; dashd `deriveaddresses "<desc>" [0,999]` gives the first 1,000 addresses of each. Compare, separately: BIP44 external (`…/44h/1h/0h/0/*`), BIP44 internal (`…/0h/1/*`) and DIP9 CoinJoin external (`…/9h/1h/4h/0h/0/*`). Core has no internal DIP9 descriptor (`scriptpubkeyman.cpp:2534-2541` builds `/0/*` only for the CoinJoin type), so key-wallet's CoinJoin internal pool (`managed_account_type.rs:34-39`) has no Core counterpart and is not compared.
     - On a legacy wallet (`descriptors=false`), the same pair through `upgradetohd`; compare 1,000 `getnewaddress` and 1,000 `getrawchangeaddress` results with BIP44 external and internal.
     - Phrases Core rejects must be refused by DashCore mode, and accepted iff `core_check`.
     - The 300-byte passphrase case on a legacy wallet must show Core's length error, and our constant must match.
  2. **Message signatures in both directions.**
     - Random keys, both compressions, and messages with empty, binary, UTF-8 multibyte and 10 KiB content. Our `sign_message` must equal `signmessagewithprivkey` byte for byte, and dashd `verifymessage` must accept ours.
     - Signatures from dashd `signmessage` on wallet addresses must pass our `verify_message`.
     - Tampered cases go through both sides: header bytes 0–255, wrong lengths, non-canonical base64. Verdicts and RPC codes must match after the §2.7 mapping.
  3. **Imported keys and scripts**, on a legacy wallet, with `rescan=false` (labels and rescans stay in WL2b). For each case, dashd's `getaddressinfo` (`scriptPubKey`, `ismine`, `iswatchonly`, `solvable`) must equal our entry (script, `ImportedKeys` vs `ImportedWatchOnly`, solving data present):
     - `importprivkey` with a compressed and an uncompressed WIF;
     - `importpubkey` with a 33-byte and a 65-byte key;
     - `importaddress` with a P2PKH address and with a P2SH address (watch-only, not solvable);
     - `importaddress` with a raw hex script, `p2sh=false` (one watched script) and `p2sh=true` (the script plus its P2SH wrapper, both watch-only);
     - `importaddress` with an address and `p2sh=true`: dashd −5; our RPC-layer check is not in WL1, so the test asserts only that dashd refuses and our `import_address` has no such flag;
     - watch-only to spendable: `importaddress` of a P2PKH address, then `importprivkey` of its key: dashd `ismine` turns true and `iswatchonly` false; our entry moves to `ImportedKeys` with `UpgradedToSpendable`.
     - **P2PK (D48):** the harness builds a transaction paying P2PK(pubkey) for an `importprivkey` key and for an `importpubkey` key, funds it, and has dashd sign it with `signrawtransactionwithwallet` from a funder wallet; after a block, dashd's `getbalances` (`mine.trusted` vs `watchonly.trusted`) and `listunspent` must equal our spendable and watch-only balances and owned outputs.

### 6.3 Property tests (proptest)

- **`core_check`:**
  - `core_check(p) ⇐ strict_bip39(p)` for every English phrase (superset);
  - equality with a direct transcription of the C++ predicate over random byte strings of 0 to 300 bytes and random 12–24 word sequences;
  - never panics.
- **Seeds:** `core_seed(p, pass) == bip39_seed(p, pass)` whenever `pass` is ASCII, at most 248 bytes, and `p` is strict. `Portability::seeds_agree` predicts equality exactly.
- **Entropy:** `from_entropy` → `parse` round-trips for 16–32-byte entropy.
- **Encryption:**
  - `encrypt` → `lock` → `unlock` → secrets equal, with `KdfParams::TEST`;
  - `change_passphrase` keeps the record bytes identical and changes the `MasterKeyRecord`;
  - flipping any single bit at any offset of a `MasterKeyRecord` or `SealedRecord`, the nonce bytes included, gives `IncorrectPassphrase` / `Corrupt` / `UnsupportedFormat`, never success;
  - swapping records between two wallets fails;
  - removing or adding a kind-2 record together with its entry gives `Tampered(RecordSet)` at unlock; a record without an entry, or a duplicate, is `Corrupt` at load (§2.5 load rules); a flipped kind-1 byte is `Corrupt`; never success (D43, D61).
- **Message signing:** `verify_message(sign_message(k, c, m)) == Ok` for all keys, compressions and messages. `decode_base64_core` equals a reference model built from Core's table over random strings.
- **Layout:** encode → decode is the identity for every `WalletType`, with and without `encryption`. Legacy bytes are unchanged (golden).

### 6.4 Functional tests: what WL1 ports

WL1 is a library, so it ports each test's library-level assertions as Rust tests. The RPC-level ports (DIFF-RPC, FT passlist) belong to WL2b, and WL1 provides the APIs they call.

| Core functional test | WL1 ports now | WL2b ports later |
|---|---|---|
| `wallet_upgradetohd.py` | the address vectors with and without passphrase; NUL passphrase gives a different seed; upgrade keeps the wallet ID and imported keys; invalid mnemonic refused | RPC flow, rescan, encrypt-on-upgrade, messages |
| `wallet_mnemonicbits.py` | `generate(bits)` word counts; invalid bits refused | `-mnemonicbits` init error text, `dumphdinfo`/`listdescriptors` fields |
| `wallet_hd.py` | path `m/44'/1'/0'/1/0` for the first internal key on regtest; restore from the same phrase gives the same keys | keypool and backup behaviour, `sethdseed` |
| `wallet_encryption.py` | state machine: encrypt → locked; wrong passphrase; empty passphrase; `relock_deadline` clamp at 100,000,000; change passphrase; NUL-byte passphrases distinct | RPC codes and messages, `unlocked_until` |
| `wallet_createwallet.py` | seedless (blank) wallet has no keys, can be encrypted, then upgraded to HD; watch-only refuses encrypt (`NoPrivateKeys`) and private-key import | the full `createwallet` argument matrix |
| `wallet_signmessagewithaddress.py`, `rpc_signmessagewithprivkey.py` | the fixed vector; P2SH gives `AddressNoKey`; invalid key and address; malformed base64 | RPC error codes and parameter-count errors |
| `wallet_watchonly.py`, `wallet_import_rescan.py`, `wallet_import_with_label.py`, `wallet_importmulti.py` (key/pubkey/address cases) | imported spendable vs watch-only routing and balances; watch-only to spendable upgrade; compression; label retention on re-import | rescans, labels, `include_watchonly`, `importmulti` warnings and the per-request result format |
| `wallet_descriptor.py`, `wallet_importdescriptors.py`, `wallet_listdescriptors.py`, `wallet_dump.py`, `wallet_migration.py` | — (descriptor engine is WL4; dumps are WL3/IM2) | yes |

### 6.5 Other

- **Disk scan.** For every step of the US-WAL-06 sequence on an encrypted wallet, the bytes `to_bytes()` produces contain none of the secrets (§5.2 item 9).
- **FFI.**
  - The header diff of `key-wallet-ffi` shows only the appended values.
  - `ffi-c-tests/header-tests` passes.
  - `dash-spv-ffi` builds and its tests pass.
- **dash-spv.** The `dashd_sync` suites pass unchanged (G0). No wallet code path changes for wallets without imports.
- **Fuzz** (DEC-184, to plateau with 0 crashes): the five targets of §5.2 item 8 are added to `node-fuzz`, or to key-wallet's fuzz dir if one is preferred, with a 5-min smoke at merge.
- **Feature matrix (D53).** The five `cargo check -p key-wallet --lib` runs of §2.6, plus `cargo check -p key-wallet-ffi` and `-p dash-spv`, in the WL1-1 gate.
- **Agentbox load.** Tests use `KdfParams::TEST`. Exactly one test (`#[ignore]`, run by the gate) exercises `KdfPolicy::desktop()` (`DESKTOP_FLOOR`) plus calibration, with a generous bound. Its timing assertions are relative (t was raised until ≥ target), never absolute.

---

## 7. Slices

Each slice is mergeable on its own, keeps G0 green, changes no existing behaviour, and has its own test bar. The order respects the dependencies (D52): WL1-1 → WL1-2 → (WL1-3 ∥ WL1-4) → WL1-5 → WL1-6 → WL1-7. WL1-3 and WL1-4 both need WL1-2's always-on `crypto/error.rs` and `crypto/params.rs`, and touch disjoint files after it; WL1-5 needs WL1-2 and WL1-3 (it signs messages with imported keys); WL1-6 needs WL1-4 and WL1-5 and comes last because it seals everything the earlier slices added.

| Slice | Content | Depends | Test bar | Review |
|---|---|---|---|---|
| **WL1-1** Layout pin + secret types | a pre-swap fixture commit, then the codec swap (§2.8): fixtures from the old derive (bincode for all five variants, plus bincode-serde `.serde.bin` snapshots, D79) with the encode/decode/re-encode/FFI-import assertions; `secret.rs` (`SecretBytes`); hand-written `Wallet` and `WalletType` Encode/Decode that is byte-identical (pinned by the new goldens), uses explicit tags with 5–7 reserved (the legacy decoder rejects 5–7 in every build, n4), and refuses WL1 features; goldens generated in key-wallet-ffi under `bincode,bls,eddsa` with BLS/EdDSA accounts (D53); `impl Drop for WalletType` (D45); the `const` trait assertion with `BorrowDecode` (D53, D80). **Implemented** on `node/wl1-1-codec` (implementation D-rows W1-1–W1-18; `thiserror` moved to WL1-2, its first user) | — | goldens pass before and after the impl swap, in key-wallet-ffi and byte-identical in key-wallet; `{:?}` redaction tests; FFI wallet-bytes round-trip tests unchanged; the `cargo check -p key-wallet --lib` matrix (four sets until WL1-4 adds `encryption`, D53); `Drop for WalletType` is a one-line call to the derived `zeroize()`, and a unit test checks that `zeroize()` wipes the `Seed`, `Mnemonic` and `ExtendedPrivKey` arms (WL1-2 adds variant 5; no test reads freed memory) | Sol (OTH); +SEC checklist items 1, 5 |
| **WL1-2** Mnemonic compat + passphrase end to end | `mnemonic/dash_core.rs` (Check/FromData/ToSeed ports, PBKDF2), `MnemonicCompat`, `WalletMnemonic`, `Portability`, `MnemonicError`; the `thiserror` 2 dependency (first user); first commit: `account_free.json` and `managed_account_free.json` on the WL1-1 head (D89); `WalletType::MnemonicWithPassphrase` [5]; `Wallet::from_mnemonic_with_passphrase`/`from_phrase`; `wallet_seed_bytes` fix; `WalletManager::create_wallet_from_mnemonic_with_passphrase`; envelope v1 for variant 5 (§2.8); zeroizing siblings, deprecations and caller migration (§2.4.1); `has_seed`/`has_mnemonic`/`can_sign` exhaustive with variant 5 (D38); `DashCoreMnemonic` codec + redacted `Debug` (D37); redacted `Debug` for `Mnemonic` and `RootExtendedPrivKey` (D40); `Mnemonic` codec on `Zeroizing` strings (D45); always-on `crypto/error.rs` + `crypto/params.rs` (D34) and `wallet/lock.rs` (`lock_state`, `require`, D52); legacy `from_bytes` leniency kept (D59) | WL1-1 | trait-coverage assertion and redacted-`Debug`/codec tests for the mnemonic types (D37); the secret-type `{:?}` scan of §2.3, chain code included (D40); default-account test with `bls,eddsa` for variants 0 and 5 (D38); `cargo check -p key-wallet --no-default-features` builds (no `encryption`, no `getrandom`) with `CryptoError::KdfOverBudget` present; Core `bip39_vectors.json`; no non-test caller of a deprecated accessor remains (clippy `-D warnings`); desktop quirk corpus (513 + 12 + dashd); `wallet_upgradetohd` vectors; proptests §6.3; empty-passphrase Bip39 still yields `WalletType::Mnemonic` with identical bytes and ID; regtest differential 1 | Sol→O-h (+SEC) |
| **WL1-3** Message signing | `message.rs` (hash, sign, verify, `decode_base64_core`, `recover_compact`); `WalletManager::sign_message` for HD keys; transcribed Core vectors + desktop cases | WL1-2 (for `CryptoError`, D52) | `util_tests` vectors byte-equal; 41 + 11 dashd cases; header 0–255 matrix; oracle `pubkey_verify` cross-check; regtest differential 2; fuzz smoke `verify_message` | Sol→O-h (+SEC) |
| **WL1-4** Encryption primitives | always-on `crypto/records.rs`: `MasterKeyRecord`, `SealedRecord`, `EncryptedWallet`, `SecretKind` data types and byte codecs (D51), kind-1 plaintext layout with the records digest and 1024-byte minimum padding (D43, D61, D63); feature `encryption`: `KdfPolicy` (write floor only, `write_params`, D44)/calibration, AEAD seal/open, argon2 with `default-features = false, features = ["zeroize"]` (D71) and an owned, fallible, wiped block buffer (D45), `relock_deadline`; strict format refusal (D30); `insecure-test-kdf` feature (D58); `argon2 0.5.3` + `chacha20poly1305 0.11` deps behind the feature. (`CryptoError`, `KdfParams` and `KdfBudget` are WL1-2's.) | WL1-2 | format known-answer tests (fixed salt, nonce, MK, passphrase → fixed bytes); tamper matrix over every byte incl. nonces; write-floor enforcement and no read floor (D44); padding (1024 minimum, size pass, no reallocation) and records-digest decode checks (D61, D63); `OutOfMemory` on a failed reservation; budget boundary tests (§2.6, argon2 never called over budget); unknown version/kind/flag/trailing-byte refusal; fuzz smoke on decoders; `cargo deny`; dash-spv `cargo tree` shows no new crate | Sol→O-h (+SEC) |
| **WL1-5** Imported, watch-only and seedless | `imported.rs`; `AccountType::{ImportedKeys, ImportedWatchOnly}`; managed accounts, routing, balances (watch-only separate), monitoring, `earliest_required_height`; `WalletType::Seedless` [6], `new_seedless`, `upgrade_to_hd`, `compute_wallet_id`/root-key arms for `Seedless` (D36); manual `Zeroize`/`Drop`/redacted `Debug` on the imported types (D35); private `imported`, `pub(crate)` mutators and the gated `Wallet::import_*` API (D41); P2PK aliases (D48); `WalletInterface::monitored_extra_scripts` + dash-spv guard, filter input/size and `all_accounts()` revision (D49, D64); import birth and the scan-cursor rewind in the import wrappers, imported births in `earliest_required_height`, anchor limit and reload duty in their API docs (D65, D77, D81, D82); derived, `serde(skip)` imported managed accounts with `sync_imported_pools` at `from_wallet`, insert, import, removal and downgrade (D81, D83); the `from_wallet` cursor seed (D90); computed `watch_only()` (D93); `ImportedScriptPool` serde/bincode impls (D97); `remove_imported` (D85); `AccountSlot` and `AccountOrigin` by slot (always compiled, D60, D72); the required `WalletInfoInterface`/`WalletInterface` methods (D78); seedless change rule in `fund`, `change_address` on the three build entry points, `None` from `next_change_address`, FFI getter refusal (D74, D84); slot `Display` (D86); `WalletManager::downgrade_to_external_signable` with managed resync (D70); `id_origin` and the seven `WalletManager` sites (D42); `downgrade_to_external_signable` drops imported secrets (D57); `ImportError`; `WalletManager` import wrappers; `sign_message` over imported keys; `Wallet::imported` with the serde strategy of §2.4; **FFI additions of §2.10** (8 arms, null pool getters, D50) | WL1-2, WL1-3 | ported import and watch-only assertions (§6.4); the `wallet_hd.py` library-level port (§6.4: first internal key path on regtest, restore gives the same keys); stored-ID test per `WalletManager` site and the re-register test (D42); P2PK ownership and mempool tests incl. raw-script-only filter, filter size and import-while-running (D48, D49, D64); import into a synced wallet with a scan in flight finds the old payment, and the below-filter-start variant finds it without a restart (D65, D77); crash-between-writes, `from_wallet` restore (incl. the payment below the given birth, D90), deserialised-insert, removal, kind-change and managed-serde tests (D81–D83, D85); seedless change tests through `build_unsigned_transaction` and the FFI getter (D84); downgrade drops secrets (D57); serde snapshot tests in key-wallet-ffi (`.serde.bin`, `account_free.json`, `managed_account_free.json`, §2.4, D79, D89); seedless ID fixtures (§2.4); redacted-`Debug` and secret-only-zeroize tests (§2.5); imported-key `sign_message` verified by `verify_message` and dashd; routing tests for every transaction type; compression; regtest differential 3; FFI header diff = appended values only; dash-spv-ffi builds; BG `filter_scan`/`address_generation` no regression | Sol→O-h (+SEC) |
| **WL1-6** Encrypted wallets + lock state | `WalletType::Encrypted` [7], always compiled (D51); sealed imported private keys (`SealedPrivateKey`, record kind 2, load rules, D41) with atomic import while Full (D62); unlock check: slot consistency, then self-derived slots, with `Tampered` naming the slot (D43, D60, D61, D72, D73); account-add gates on `Encrypted` and the `encrypt`/upgrade origin preconditions (D60); `imported_signing_key` via `require` (D67); migrate-on-unlock and `UnlockOutcome` (D44); `Wallet::{encrypt, unlock, lock, lock_to_mixing_only, change_passphrase}` with Core's lock cases (D46); locked clones and removal of the `lib.rs:298-299` clone (D56); `downgrade_to_external_signable` on `Encrypted` (D57); `SecretUse` gating on every secret-reading path, incl. the deprecated accessors (§2.4.1); `upgrade_to_hd_encrypted` (§2.6.1) with Core's error order and the bad-phrase relock (D47); seedless kind-1 record (D33); `compute_wallet_id`/root-key arms for `Encrypted` (D36); `EncryptedWallet` `Clone`/`Zeroize`/`Drop`/redacted `Debug` (D37); `has_seed` for `Encrypted` by `SecretKind` (D38); `WalletManager` wrappers; envelope v1 for encrypted wallets | WL1-4, WL1-5 | state machine from `wallet_encryption.py`/`wallet_createwallet.py`; mixing-only rules; encrypted-upgrade tests of §2.6.1 (precondition failures keep the lock state; later failures and success end Locked; reseal, atomicity, imported-key retention, ID preserved; blank → encrypt → upgrade round trip); seedless kind-1 decode tests (§2.6); encrypted ID fixtures (§2.4); trait-coverage assertion incl. `EncryptedWallet` and its redacted-`Debug` test (D37); sealed-lifecycle, import-while-encrypted and load-rule tests (D41); one tamper test per `TamperKind` and the kind-1 `Corrupt` case (D43, D61); the origin, relabelled-slot and re-add tests (D60, D72, D87); slot-redaction test (D86); `assert_codec::<EncryptedWallet>` in WL1-4 (D88); RNG-fault import atomicity (D62); floor-upgrade tests (D44); per-case lock-state tests incl. `change_passphrase` from MixingOnly ending Full (D46, D66); upgradetohd error-order tests (D47); locked-clone test (D56); default-account test for encrypted-HD wallets with `bls,eddsa` (D38); locked extraction returns `None`/`Err(Locked)`; disk-scan test; locked paths return `InvalidState` through the unchanged FFI; one `desktop()` policy test (ignored by default) | Sol→O-h (+SEC) |
| **WL1-7** Hardening pass | fuzz targets to plateau; tracing-capture secret test across the suite; clippy `disallowed-methods` snippet documented for WL2a; midstate HMAC for the PBKDF2 path; the `restore` decode bound; the copies made by `backup()` and the serialize path (`key-wallet-manager` `encode_to_vec`); the full constant-time path (possible security note: see manager); README/CLAUDE.md for key-wallet updated (also fixes the stale `Wallet.accounts` note at `key-wallet/CLAUDE.md:44`) | WL1-1…6 | fuzz plateau 0 crashes; capture test green; G-SEC sign-off on the whole WL1 | O-h with `security-review` |

Sizes: WL1-1 S, WL1-2 M, WL1-3 S, WL1-4 S, WL1-5 M/L, WL1-6 M, WL1-7 S. The total sits at the row's L.

---

## 8. Decisions (D-rows)

| # | Decision | Rationale |
|---|---|---|
| D1 | Cite Core at develop `15db47875`; take vectors from the vendored rc.3 directory `test-vectors/core/0364ed626ec2/` | The task fixes the checkout. The source files and the vector/assertion sections WL1 uses are identical at the program pin (header table lists exactly which; `wallet.cpp`, `util_tests.cpp` outside the message tests, and `wallet_migration.py` differ and contribute no vectors), so vectors stay on the pin's provenance chain |
| D2 | `Wallet::from_mnemonic_with_passphrase(Bip39 mnemonic, "")` returns `WalletType::Mnemonic`; variant [5] is used only for a non-empty passphrase or DashCore mode | Keeps today's bytes and IDs for the common case. Existing iOS/Android blobs and the new API agree |
| D3 | The library default is `MnemonicCompat::Bip39`. The node uses `DashCore` at every mnemonic entry point (`-mnemonic`, `upgradetohd`, generation, import), not only for imports | US-WAL-02: "Phrases that dashd accepts work here too" and the same addresses as dashd. This refines A1 §2.20 item 1's "for imports only" (the manager should confirm). Mobile and Platform behaviour is unchanged |
| D4 | Passphrases are raw bytes (`SecretBytes`), not `&str` | Core stores and hashes raw bytes, NULs included (`wallet_upgradetohd.py:258-267`). Bip39 mode checks UTF-8 at use |
| D5 | `core_seed` ports the 256-byte salt cut verbatim; the legacy-kind 256-byte rejection is a caller constant enforced by WL2a | Core enforces the limit only on the legacy path (`hdchain.cpp:60-62`), and the descriptor path silently truncates. Both kinds must reproduce Core |
| D6 | Two-level encryption: argon2id KEK wraps a random MK; secrets sealed under the MK with XChaCha20-Poly1305 | Core's `mkey` model (cheap passphrase change, one KDF per unlock) and the desktop vault's shape. A1 §2.20 item 3 names the primitives |
| D7 | Random 24-byte nonces, AAD binding magic, version, kind, record id, KDF header and wallet id; no nonce counters | 192-bit nonces make collisions negligible without state. AAD prevents parameter downgrade and record swaps |
| D8 | Two policies: `desktop()` with write floor 256 MiB / t = 3 / p = 1 (used by `dashd-wallet`; matches dw-vault, which passed its own review) and `default()` with floor 64 MiB / t = 3 / p = 1 for mobile consumers; both calibrated to ≥ 500 ms (t ≤ 24); absolute ceiling 4 GiB / 64 / 16. The floors bind writes only (D44) | Both floors are far above FR-WAL-04's Core baseline. A mobile-safe library default keeps iOS/Android viable without opting out of anything. The ceiling is a sanity bound, not DoS protection (D28) |
| D9 | Imported keys keep the WIF compression flag; P2PKH script from the key as imported | Core behaviour; addresses must match dashd |
| D10 | Seedless wallets get a random stored ID; `upgrade_to_hd` keeps the ID, and the envelope records `id_origin = Stored`, held in memory as `Wallet::id_origin` (D42) | A wallet's identity must survive `upgradetohd`; WalletManager maps and the persistence layers key on it |
| D11 | key-wallet holds no clocks or timers: unlock timeouts and relocking are the caller's, with a pure `relock_deadline` helper carrying Core's rules. `ImportBirth::Time` is resolved to a height by the caller | Pure library, deterministic tests. The node (WL2a) and desktop already own schedulers. Avoids `Instant` in serialised types |
| D12 | Mixing-only unlock gates by operation (`SecretUse::{CoinJoin, KeyPoolTopUp}` allowed), not by derivation path | Core: the key store serves `IsLocked(true)` callers (CoinJoin, top-up), and legacy wallets mix on the BIP44 external chain, so a path rule would be wrong for imported legacy wallets |
| D13 | Message signing lives in key-wallet with Core-exact header arithmetic and a `DecodeBase64` port. The `dash` crate is not modified | WL1's crates are key-wallet and key-wallet-manager (PLAN row). `dash::sign_message`'s stricter `from_slice` stays as it is for its existing users (ProTx payload checks) |
| D14 | FR-WAL-15 reading: the library never writes secrets in plaintext implicitly. An unencrypted wallet's secrets are plaintext in its file only because the user chose no encryption (Core parity: `createwallet` without passphrase). Exports happen only on explicit request | Core must be drop-in (unencrypted wallets are its default). The manager should confirm this reading of FR-WAL-15 |
| D15 | No oracle extension for BIP39 or message signing; differential runs against regtest dashd plus the oracle's `pubkey_verify` | The oracle is consensus-scoped (A1 §4.4). The wallet functions are fully observable through dashd RPCs, deterministically |
| D16 | New crypto dependencies (argon2, chacha20poly1305) sit behind key-wallet feature `encryption`, default off; key-wallet-manager forwards it. The record data types and `WalletType::Encrypted` are always compiled; only the crypto is gated (D51 supersedes rev3's "decoding fails without the feature") | dash-spv, the FFI crates and their benches keep an unchanged dependency graph (PLAN BG). The node and desktop opt in |
| D17 | No new variants in `key_wallet::Error` or `WalletError`. New typed errors (thiserror 2, `#[non_exhaustive]`) are used by new APIs only. A locked wallet on an existing path returns `Error::InvalidState("wallet is locked")` | `key-wallet-ffi/src/error.rs:242` matches exhaustively, so a new variant would be an FFI source change. "FFI unchanged" |
| D18 | `SecretBytes` gets bincode but no serde implementation; the new `WalletType` variants are `serde(skip)` and a non-empty `Wallet::imported` refuses to serialise through serde | Serde output goes to whatever serde format a consumer uses (JSON where it can encode the wallet, bincode-serde, logs); the guarantee lives in the `Serialize` impls, so it holds for every format (tests: D79). New secrets cannot leave through serde, and refusing (rather than skipping) means a serde round trip never silently drops a passphrase or an imported key. bincode is needed only for the wallet layout and envelope |
| D19 | No new `WalletEvent` variants in WL1 | dash-spv-ffi matches `WalletEvent` exhaustively. Lock-state notifications belong to WL2a |
| D20 | Encrypted wallets keep `root_xpub` and account xpubs in clear; they are unauthenticated while locked and compared against the sealed secrets at every unlock (D43) | Locked wallets must keep tracking and deriving receive addresses (Core descriptor caches do the same). Public keys are not secrets under the threat model |
| D21 | The desktop receives WL1 only through upstreamed rust-dashcore releases; nothing is copied into dashwallet-desktop from `node/integration` | DEC-09 (node branches never pushed) and DEC-13 (desktop is public). It is consistent with PLAN §5 SW-DESK |
| D22 | Imported keys are two `AccountType`s (spendable and watch-only), one each per wallet, not per-import accounts | Core's legacy key store is one set with two ownership levels, and balances report watch-only separately. Two fixed kinds keep the FFI addition to two values |
| D23 | Imported account kinds are not stored in `AccountCollection`; the public and secret data are in `Wallet::imported`, the funds state in `ManagedAccountCollection` (derived `serde(skip)` accounts, D81, D83) | `Account` requires an xpub and `AccountCollection` is a pinned bincode struct. rev1–rev6 used serde-default fields, which non-self-describing formats cannot default (D83) |
| D24 | Wallets with WL1 features persist only through envelope v1 (`0xFF ‖ "KWW" ‖ 1 ‖ …`). Legacy encoders refuse them rather than silently dropping data | The layout pin (A1 §2.24) is kept for every existing wallet. Old readers fail loudly on new files. Imports can never be lost by a legacy `backup()` |
| D25 | Desktop Core-generated vector files are vendored verbatim with provenance, not regenerated | They are produced by Core's `bip39.cpp` and dashd v24.0.0 and already cover the quirk edges (weak checksum, NUL, NFKD, 248/249/256). Regeneration scripts are linked for audit |
| D26 | `WalletType` gets a hand-written codec with explicit, reserved tags (0–4 existing, 5 `MnemonicWithPassphrase`, 6 `Seedless`, 7 `Encrypted`) | Slices add variants in different merges and `Encrypted` is feature-gated; derive tags follow declaration order and `cfg`, which would let a merge order or a feature flag change stored bytes |
| D27 | Existing seed and phrase accessors keep their signatures; WL1 adds zeroizing siblings, deprecates the public bare-copy accessors, migrates every internal caller, and gates extraction on encrypted wallets. `Seed` stays `Copy` | A narrow, non-breaking change. Removing `Copy` or changing return types would break iOS/Android/Platform consumers; deprecation steers them without a break, and the zeroization claim is scoped to what WL1 actually covers |
| D28 | Readers apply a pre-authentication KDF budget before argon2 runs. Library default `KdfBudget::MOBILE` (m ≤ 64 MiB, t ≤ 24, p ≤ 4) with `KdfPolicy::default()` writing 64 MiB files; `KdfPolicy::desktop()` (budget 256 MiB, used by `dashd-wallet`); a caller override up to the 4 GiB / 64 / 16 ceiling; `KdfOverBudget { required }` lets the caller ask the user | The parameters are unauthenticated until the KDF has run, so the reader must choose its own limit. A mobile-safe default keeps any consumer from being stalled by a file it did not write; each policy always opens what it writes; the override is the recovery path for legitimate high-cost files |
| D29 | FFI consumers get no WL1 capability in WL1; follow-up WL1-F (passphrase constructors, encryption/unlock calls, `WalletLocked` error code, message signing) is recommended as a BACKLOG row | WL1's consumer is the Rust node wallet; mobile FFI design needs its owners. The limits are stated, not hidden (§0 item 7, §2.10) |
| D30 | Readers refuse any unknown version, kdf id, record kind, flag bit or trailing data with `UnsupportedFormat`; nothing is preserved or skipped. New kinds require a version bump | Simpler than an opaque-record container, and nothing writes unknown records yet. Refusing the whole file means an old reader can never load and rewrite a wallet minus records it did not understand |
| D31 | `upgrade_to_hd_encrypted` builds the new state off to the side and commits it in one step; once past its preconditions (D32) it ends `Locked` on every outcome and, on failure, byte-identical to before | Core relocks after a successful encrypted upgrade; a library operation that leaves state half-changed or unlocked on error is harder to reason about for callers |
| D32 | `upgrade_to_hd_encrypted` checks its preconditions (AlreadyHd, PrivateKeysDisabled, NotEncrypted, EmptyPassphrase) before touching the lock state; a precondition failure leaves the lock state and bytes as they were. Only calls past the preconditions relock → unlock → install → relock and end `Locked` | Core `wallet.cpp:373-430` throws on these checks before `Lock()`, so an unlocked wallet stays unlocked; rev1's "ends Locked on every outcome" contradicted both its own step order and Core (Sol r2 M1) |
| D33 | An encrypted seedless wallet has a kind-1 record whose plaintext is `WalletType` variant 6 with an empty payload; `upgrade_to_hd_encrypted` replaces it with a fresh-nonce variant 0 or 5 record in its single commit | Keeps the envelope rule "exactly one kind-1 record per encrypted wallet" with no special case, and makes blank → encrypt → upgrade representable. The upgrade writes the HD variant that D2 picks (0 for an empty Bip39 passphrase, otherwise 5) |
| D34 | `KdfParams` and `KdfBudget` (types and constants only, no argon2) live in the always-on `crypto/params.rs`; `KdfPolicy` and the crypto stay behind `encryption` | `CryptoError` is always on (it is returned by `seed_zeroizing`) and names `KdfParams` in `KdfOverBudget`; the alternative, gating that variant, would make the error enum differ by feature |
| D35 | `ImportedKeys`, `ImportedEntry` and `ImportedKind` have no `Zeroize`/`ZeroizeOnDrop` derives; manual `Zeroize` wipes only the private-key bytes, `Drop` is on the container, and a manual `Debug` redacts the secret | `ScriptBuf`, `PublicKey` and `BTreeMap` have no `Zeroize` impl, so the derives would not compile, and they are public anyway; `Wallet` keeps its derived `Debug`, so the imported types must redact themselves |
| D36 | `compute_wallet_id` returns the stored ID for `Seedless` and `Encrypted`; the root-key accessors return `InvalidParameter`/`InvalidState` for them (public root from `root_xpub` where present) and never `expect`; the `_ =>` fallback becomes an explicit list | Both variants are built or imported without a root key on hand; the old fallback would panic or recompute a different ID |
| D37 | Every type reachable from `WalletType`/`Wallet` implements their derive set: `DashCoreMnemonic` gets a manual bincode codec (decode re-runs Core's Check) and a redacted `Debug`; `EncryptedWallet` gets `Clone` (rev3: unlocked clones unlocked; superseded by D56, clones are locked), `Zeroize` (= lock) + `Drop`, a redacted `Debug` and an envelope-only codec; a compile-time trait test pins it per feature set | The retained derives on `WalletType`/`Wallet` otherwise fail to compile (Sol r3 M1). The rev3 clone rule kept `Wallet: Clone` transparent (WalletManager clones on insert, `key-wallet-manager/src/lib.rs:299`); D56 replaces it and removes that clone |
| D38 | `has_seed()` is true for variants 0, 1, 5 and for `Encrypted` with a mnemonic or seed `SecretKind`, independent of lock state; default BLS/EdDSA accounts are created from plaintext before sealing and never need an unlock; `has_mnemonic`/`has_seed`/`can_sign` become exhaustive matches | `create_accounts_from_options` gates the BLS operator and EdDSA platform accounts on `has_seed()`; omitting variant 5 would silently drop them for passphrase and DashCore wallets (Sol r3 M2). The other predicates were swept and are correct |
| D39 | The imported types keep a manual redacted `Debug` even though zeroize 1.9.1's `Zeroizing` prints `Zeroizing { .. }` | Defence in depth: the redaction is ours and does not rely on a dependency's `Debug` (Sol r3 m1 corrected the rationale) |
| D40 | `Mnemonic` and `RootExtendedPrivKey` get a manual redacted `Debug` in WL1-2; a `{:?}`/`{:#?}` scan over every secret-bearing type counts the chain code as secret. `Display` of existing types is unchanged and outside WL1's claims | bip39 2.2.2's `Mnemonic` Debug prints only the language, so for `Mnemonic` this is hardening against a future bump, not a fix; `RootExtendedPrivKey` is brought to the same redaction standard (Opus r1 ruling, m13). No consumer reads either format |
| D41 | Imported private keys have two in-memory forms: `PrivateKey` (unencrypted wallets only) and `SealedPrivateKey { pubkey, record }` (encrypted wallets only). `Wallet.imported` is private with `pub(crate)` mutators; `Wallet::import_*` apply the wallet-kind and lock gates (Locked/MixingOnly refuse private imports; Full seals at once); `lock` leaves sealed entries; `to_bytes` writes one kind-2 record per sealed entry; load rules refuse missing, duplicate and orphan records; signing decrypts on demand | Opus r1 M1: the rev3 model had no locked form, so locking would have kept plaintext keys or a save would have dropped them, and a public field bypassed the gates. Core holds imported keys as `ckey` records under the MK and refuses `importprivkey` while locked (−13) |
| D42 | `Wallet` gains an in-memory `id_origin: IdOrigin { Derived, Stored }`; `compute_wallet_id()` returns `wallet_id` for `Stored`; every `WalletManager` site (`accessors.rs:64`, `lib.rs:287, 372, 411, 452, 500, 548`) keeps calling it and is tested with a stored-ID wallet; a re-register test rebuilds a manager from bytes | Opus r1 M2: the manager re-derives IDs at seven sites, so the D10 stored ID was lost on insert, import and re-register. Making the one function origin-aware fixes all sites at once |
| D43 | Every passphrase-verifying unlock re-derives `root_xpub`, the xpub or BLS/EdDSA key of every self-derived slot (D60, D72), and every imported pubkey/script from the sealed secrets and compares; a mismatch is `CryptoError::Tampered(TamperKind)` and the wallet stays Locked. The kind-1 plaintext carries `records_digest` over the count and sorted ids of the kind-2 records (rev4 put it in the AAD; D61). The threat model names what is unauthenticated while locked. Every account's slot consistency is checked (D72) | Opus r1 M3: rev3 claimed tamper protection the public half did not have. The comparison is linear and cheap next to argon2. Binding the record set to kind 1 costs one hash and a reseal per kind-2 change, so it is done; public watch-only data stays unauthenticated, as in Core |
| D44 | No KDF floor on reads: only the ceiling, the budget and argon2's own minimums. The policy floor applies to `encrypt`, `change_passphrase` and migrate-on-unlock, which re-wraps a below-floor MK at `write_params()` and reports `kdf_upgraded`; never downwards | Opus r1 M4: a read floor locked desktop users out of mobile-written files and adds nothing, since lowered parameters cannot unwrap the MK (they are in the AAD) and a forged file costs the attacker nothing. Upgrading on unlock gives the desktop its 256 MiB without a migration step |
| D45 | `impl Drop for WalletType` (calls the derived `zeroize`); `EncryptedWallet::lock`/`Drop` wipe the MK and plaintext explicitly; argon2's `zeroize` feature plus `hash_password_into_with_memory` over an owned, `try_reserve_exact`-allocated, wiped buffer (`OutOfMemory` on failure); `CEILING.m` clamped to what one allocation can address; the `Mnemonic` codec uses `Zeroizing` strings; the kind-1 buffer is pre-sized | Opus r1 m1–m3: `WalletType` has no `Drop` today (rev3's rationale was wrong), argon2 0.5.3 frees its blocks unwiped, and the existing codec copies the phrase into plain `String`s on every seal and unseal |
| D46 | `unlock` follows Core's `Unlock`/RPC order: MixingOnly while Full → `AlreadyUnlocked`; empty → `EmptyPassphrase`; Full while Full → Ok without verification; from Locked or MixingOnly, verify. `lock_to_mixing_only` = Core `Lock(fAllowMixing=true)`. `change_passphrase` ends Locked→Locked, MixingOnly→Full, Full→Full; a wrong old passphrase leaves it Locked | Opus r1 m4: the four cases were unspecified or different. Core `wallet.cpp:4158-4229`, `:576-617`, `rpc/encrypt.cpp:60-90` |
| D47 | `upgrade_to_hd*` take the phrase as `&str` + `MnemonicCompat` and parse it after the passphrase check, so errors come in Core's order (AlreadyHd, PrivateKeysDisabled, empty passphrase, wrong passphrase, bad phrase). Every failure after the unlock, including a bad phrase, ends Locked | Opus r1 m5. Matching Core's error precedence keeps RPC behaviour identical; a relocked wallet is the safe end state for a failed call. possible security note: see manager |
| D48 | Every imported key and pubkey entry also owns P2PK(pubkey) as an alias: spendable when a private key is held, watch-only otherwise | Opus r1 m6: Core's `importpubkey` adds `GetScriptForRawPubKey` as watch-only (`scriptpubkeyman.cpp:1763-1781`) and IsMine's `PUBKEY` case is SPENDABLE for a held key (`:101-107`) |
| D49 | `WalletInterface::monitored_extra_scripts()` (required since rev6, D78) returns raw imported scripts and P2PK aliases; dash-spv's mempool bloom filter inserts their data pushes (guard, sizing and rebuild completed by D64). Scripts with no push are seen at confirmation only, documented | Opus r1 m10: `monitored_addresses() -> Vec<Address>` cannot carry raw scripts, so the mempool never saw them. rev4–rev5 gave it a default empty body; rev6 makes it required so that no implementor silently drops the filter input (D78) |
| D50 | key-wallet-ffi gets arms at all 8 exhaustive sites; the address-pool getters return null for the imported kinds; Platform's exhaustive matches are named in §0/§4.4 as an accepted upgrade cost; the public enums are not made `#[non_exhaustive]` | Opus r1 m7 probe. Under 0.x an appended variant is an accepted break, and exhaustive enums make the compiler list every downstream site; `#[non_exhaustive]` would turn missed arms into silent `_` fallbacks |
| D51 | `WalletType::Encrypted` and the record data types are always compiled; only the crypto is behind `encryption`. Without the feature an encrypted file decodes, stays Locked, re-encodes byte-equal and derives public keys | Opus r1 m7: a `cfg`-gated variant is non-additive, so enabling the feature anywhere in a graph broke downstream exhaustive matches. Supersedes D16's decode-fails rule |
| D52 | WL1-3 and WL1-4 depend on WL1-2; WL1-4 does not re-list `CryptoError`/`KdfBudget`; `lock_state`/`require` are always compiled in `wallet/lock.rs` (WL1-2) | Opus r1 m8: always-on code calls `require`, and two slices claimed the same files |
| D53 | The trait-coverage check is a lib-side `const` item; the per-feature matrix is five `cargo check -p key-wallet --lib` runs; runtime tests use the unified dev-dependency set (BL-104 owns removing the self dev-dependency); goldens (bincode layout `.bin`, plus bincode-serde `.serde.bin` checked in key-wallet-ffi, D79) are generated in key-wallet-ffi under `bincode,bls,eddsa` with BLS/EdDSA accounts and asserted byte-identical in key-wallet | Opus r1 m9: the self dev-dependency forces `test-utils, serde, bincode, eddsa, bls` into every test build, so `cargo test` per feature set was one build; the layout depends on `bls`/`eddsa`, and iOS writes under key-wallet-ffi's defaults |
| D54 | The kind-1 plaintext is zero-padded to a multiple of 256 bytes, the padding authenticated and checked on decode; the minimum bucket is 1024 bytes (D63) | Opus r1 m11a: the ciphertext length revealed the BIP39 passphrase length. rev4's "256 covers a typical passphrase" was wrong for 24 words; see D63 |
| D55 | `change_passphrase` keeps the MK (Core parity) and §5.1 states the consequence; no `rekey()` in WL1 | Opus r1 m11b. Core has no rekey; a compromised passphrase is answered by a new wallet and a sweep, which WL2b/desktop already offer. A rekey would be new scope with its own failure-atomicity design |
| D56 | Cloning an `EncryptedWallet` yields a Locked copy; WL1-6 removes the clone at `key-wallet-manager/src/lib.rs:298-299` | Opus r1 m12: lock state is per copy, so an unlocked clone would outlive `lock()` on the manager's copy. Supersedes D37's clone rule |
| D57 | `downgrade_to_external_signable` keeps its signature and drops every secret: private imported entries become `PublicKey(pubkey)` watch-only entries (secrets wiped, kind-2 records dropped); on `Encrypted` the MK and kind-1 records go too; `id_origin` is kept | Opus r1 m14: it left imported keys behind, contradicting "ExternalSignable accepts only watch-only". Converting rather than refusing keeps the iOS keychain flow working |
| D58 | `KdfPolicy::allow_weak_for_tests()` sits behind `cfg(any(test, feature = "insecure-test-kdf"))`, a feature no crate forwards | Opus r1 n5: `test-utils` is reachable from dash-spv as a normal feature |
| D59 | `from_bytes`'s legacy branch keeps today's trailing-byte leniency; only the envelope is strict | Opus r1 n6: `import_wallet_from_bytes` ignores trailing bytes today (`lib.rs:540-545`); refusing them could reject files that load now, and the legacy layout gains nothing from it |
| D60 | Each account slot has a computed `AccountOrigin`, taken from the slot and never from the stored `account_type` (D72): `External` (`DashpayExternalAccount`), `CallerSupplied` (every account of a `Seedless` wallet; BLS/EdDSA accounts of an `ExtendedPrivKey` wallet) or `SelfDerived`. The unlock check compares only `SelfDerived` accounts; the other two are listed in §5.1 as unauthenticated. On an `Encrypted` wallet, `External` adds are allowed in any lock state, `SelfDerived` adds need Full and must match the wallet's own derivation, and `CallerSupplied` adds are refused for every funds-bearing type. `encrypt` refuses a wallet holding a mismatched `SelfDerived` account (`ForeignAccountKey`); `upgrade_to_hd*` refuse a `Seedless` wallet with non-`External` accounts (`CallerSuppliedAccounts`) | Opus r2 M1: rev4's check flagged DashPay contact xpubs, explicit-seed BLS/EdDSA on variant-2 wallets and seedless xpub accounts as `Tampered`, locking those wallets out for good. Computing the origin from the slot and the sealed kind means a file edit cannot exempt a self-derived slot (rev5 read the stored `account_type`, which a file edit can relabel; corrected by D72 after Opus r3 M1); refusing unverifiable spending-account adds keeps the exempt set from growing while encrypted |
| D61 | `records_digest` moves from the kind-1 AAD into the kind-1 plaintext (after the `WalletType` encoding, before the padding). A kind-1 tag failure is `Corrupt`; an opened record whose digest differs from the recomputed one is `Tampered(RecordSet)` | Opus r2 m1: under the AAD placement both cases were the same AEAD failure, so no reader could pass both rev4 tests. Same cost and strength, distinguishable errors |
| D62 | `import_private_key` while Full stages the kind-2 record, the entry and the resealed kind-1 record, then commits them together; any failure leaves `self` unchanged | Opus r2 m2: a failed kind-1 reseal after the kind-2 insert left a saved wallet that could never unlock. Same discipline as §2.6.1 |
| D63 | The kind-1 padding minimum is 1024 bytes (multiples of 256 above it); the padded size comes from a bincode size pass before allocation | Opus r2 m3: with 256 a 24-word variant-5 wallet still leaked whether its passphrase exceeds ~35 bytes. 1024 holds 24 words plus a 256-byte passphrase (≈ 574 B with the digest) for under 1 KiB per wallet |
| D64 | dash-spv's mempool path gets the full D49 wiring: the early-return guard counts extra-script pushes, `build_wallet_bloom_filter` takes and sizes for them, and the imported managed accounts are in `all_accounts()`/`all_accounts_mut()` with a revision bump on insert | Opus r2 m4: raw-script-only wallets loaded no filter and runtime imports never triggered a rebuild |
| D65 | WL1-5 owns the rescan signal: import wrappers record the import birth (0 when unknown) and rewind the one scan cursor through the #649 path (`rewind_scan_cursor`: synced_height to birth − 1, `account_generation` bumped; D81 replaced rev5's lowered `metadata.birth_height` and rev6's `imports_floor`). dash-spv needs no change when the new birth lies in its stored range (tested in WL1-5); a birth below the stored filter start but at or above the header anchor is handled by the existing tick at runtime (D77); only a birth below the anchor is a backlog row owned by dash-spv sync | Opus r2 m5: `scan_start = max(birth, committed + 1)` ignores a lowered birth after the first sync, so imported keys' history was never scanned. Reusing #649's path keeps the dash-spv change to the existing behind-wallet backfill; the rare out-of-range case needs filter re-download logic that belongs to the SPV track |
| D66 | `change_passphrase` uses `was_locked = (state == Locked)` (Core `IsLocked(true)`) | Opus r2 m6: rev4's `state != Full` relocked a mixing-only wallet, contradicting its own end-state table and Core `wallet.cpp:578` |
| D67 | `imported_signing_key(script, use_: SecretUse)` calls `require(use_)`, so MixingOnly admits CoinJoin/KeyPoolTopUp for imported keys as for HD keys | Opus r2 n1: `KeyUse` was undefined. D12 gates by operation, not key origin; Core's mixing-only unlock serves every key in the store |
| D68 | `ImportedKeys::unseal_all` is dropped; `downgrade_to_external_signable` is not a kind-1 reseal | Opus r2 n2, n3: WL1 (like Core) has no decrypt-to-plain operation, and D57 drops kind 1 rather than resealing it |
| D69 | §2.10 lists that an FFI handle to an encrypted wallet is always Locked (`wallet_manager_get_wallet` clones); the D56 guarantee rests on the locked `Clone`, and removing the `lib.rs:299` clone is cleanup | Opus r2 n4 |
| D70 | `WalletManager::downgrade_to_external_signable(wallet_id)` calls `sync_imported_pools`, which moves each converted script with its UTXOs and transaction records to `ImportedWatchOnly` (D81); callers of the bare `Wallet` method call it themselves | Opus r2 n5: the managed side was unspecified. Platform, the only caller today, has no imported keys in WL1 |
| D71 | argon2 is declared with `default-features = false, features = ["zeroize"]` | Opus r2 n6: the defaults pull password-hash, base64ct and rand_core, which WL1 does not use; this keeps the WL1-4 dependency bar small |
| D72 | `AccountOrigin` and the derivation path the unlock check uses come from the account's **slot** (`AccountSlot`: map plus key, or single-account field), never from the stored `account_type`. A slot-consistency rule (stored `account_type` == slot type, stored `network` == wallet network) runs at every unlock (`Tampered(AccountXpub(slot))` / `SpecialAccountKey(slot)`) and in the `encrypt` precondition (`ForeignAccountKey(slot)`). API: `account_origin(&self, slot: AccountSlot)`, `AccountSlot::account_type()`, `AccountCollection::slots()`. `WatchOnly`/`ExternalSignable` slots are `CallerSupplied`; imported kinds have no slot (n4) | Opus r3 M1: rev5 classified by the stored type, so relabelling a BIP44 or operator-key slot `DashpayExternalAccount` exempted a foreign key from the check while consumers kept reading it by slot. The slot fully determines the type; the rule is free on every unlock; relabel tests for both slots |
| D73 | `Wallet.accounts` stays public (option b). `TamperKind::AccountXpub`/`SpecialAccountKey` carry the `AccountSlot`, so a caller can remove the offending account through the field and unlock again. Writing the field directly is outside the D60 guarantee; §5.1 says the add gates cover only `add_account`/`add_bls_account`/`add_eddsa_account` | Opus r3 m1: making the field `pub(crate)` is a wider 0.x break than WL1 needs; naming the slot turns a permanent lockout into a recoverable one |
| D74 | §5.1 names both consequences of unauthenticated accounts: payments to a tampered `External` contact go to the tamperer, and `CallerSupplied` receive addresses (and any change taken from them) can be redirected. Seedless spends take an explicit change `Address` from the caller (Core's `changeAddress`; a blank wallet has no keypool) and never select a `CallerSupplied` account for change; the guard sits in `fund` and `next_change_address` returns `None` (D84). Re-checking a DashPay contact's xpub against the contact request is a note for Platform (§4.4), outside WL1 | Opus r3 m2 |
| D75 | §5.1 states that the unlock check covers the `Wallet` envelope only; WL2a must rebuild or verify `ManagedWalletInfo`'s cached pool scripts from the authenticated xpubs after unlock and at load, before handing out any address (§4.2) | Opus r3 m3: the pools are persisted separately and a tampered pool record would redirect addresses without touching `Wallet.accounts` |
| D76 | **Superseded by D81 (rev7).** Imports lower a separate `WalletMetadata::imports_floor: Option<CoreBlockHeight>` (`serde(default)`); `earliest_required_height` and the import rewind use `min(birth_height, imports_floor)`, while the account-add rewind keeps flooring at the HD birth. `WalletManager` re-runs `lower_imports_floor_and_rewind(imported().earliest_birth())` whenever it inserts or loads a wallet with its managed info, so a crash between the wallet and managed-info writes heals at the next load | Opus r3 m4: lowering `metadata.birth_height` made every later contact add rescan from the import's birth (0 for `Genesis`), and two separately persisted objects could lose the rewind |
| D77 | The backlog row is narrowed to a birth below the header anchor. A birth below the stored filter start but at or above the anchor is handled by the existing `FiltersManager::tick` → `reset_for_rescan` → `start_download` path while running; WL1-5 adds a regtest variant for it. The import wrappers' API docs state the anchor limit | Opus r3 m5: `tick` already re-downloads from the anchor (`filters/sync_manager.rs:213-247`, `filters/manager.rs:189, 265-283`); only headers below the anchor are missing |
| D78 | The new managed-side hooks are **required** trait methods with no defaults: `WalletInfoInterface::{monitored_extra_scripts, sync_imported_pools, rewind_scan_cursor}` (three since rev7, D81) and `WalletInterface::monitored_extra_scripts`. §4.4 lists Platform's three `PlatformWalletInfo` delegation arms next to the D50 cost | Opus r3 m6: `WalletManager<T>` reaches the managed info only through traits; a no-op default would make `WalletManager<PlatformWalletInfo>` imports skip the rescan and the mempool filter silently. Required methods make the compiler name every implementor |
| D79 | The five-variant serde pins are bincode-serde `.serde.bin` snapshots (WL1-1, W1-3), checked in key-wallet-ffi; one serde_json snapshot of an account-free variant-0 wallet (`account_free.json`, no DashPay or platform-payment accounts) is added by WL1-5's first commit and covers the injected-key test, the human-readable path and "exactly the old JSON" | Manager ruling on W1-3 and Opus r3 m7: serde_json cannot encode struct-keyed account maps, and every default wallet has a `PlatformPayment` account; a positional format cannot express an injected key |
| D80 | The `const` trait assertion requires `Encode + Decode<()> + for<'de> BorrowDecode<'de, ()>`; every secret type's `BorrowDecode` delegates to `Decode` and never borrows from the input | Manager ruling on W1-13 and Opus r3 n6: a derived `Decode` on any containing type also derives `BorrowDecode`, which needs it on every field; delegating keeps decoded secrets from aliasing the caller's buffer |
| D81 | **One scan cursor, derived imported pools (supersedes D76).** `imports_floor` and the load-time self-heal are dropped. An import rewinds the wallet's existing cursor: `rewind_scan_cursor(birth)` sets `synced_height = min(synced_height, birth − 1)`, with birth 0 when unknown. `earliest_required_height` takes the lower of the HD birth and the earliest birth in `Wallet::imported()`, read from the wallet itself. The two imported managed accounts are `serde(skip)` derived state, reconciled by the required `sync_imported_pools` (add missing, move on kind change, drop orphans) in `from_wallet`, at every `WalletManager` insert, and after every import, removal and downgrade. WL2a persists the lowered cursor before the new entry, both before the call returns; this is the only ordering contract. `from_wallet` seeds the cursor at `min(birth_height, earliest import birth) − 1` (D90) | Manager ruling on Opus r4 M1 and m2: Core has no load-time heal; it rescans on import, and an interrupted rescan is rerun with `rescanblockchain`. The rev6 heal missed a lost rewind above the floor, scanned without the new scripts and latched, and never rebuilt pools at load; persisted pools could also hold orphans. With the pools derived from the authority and the cursor written first, every crash ordering is either an extra rescan or an import that never happened. The imported births have to reach `earliest_required_height`, because dash-spv floors a restarted scan there (`filters/sync_manager.rs:218-224`) |
| D82 | The rebuilt imported accounts start without UTXOs or transaction records. WL2a restores them by replaying its `txs` records (A1 §2.20) through the transaction-checking path after the rebuild, ordered by D91. A consumer that persists only `ManagedWalletInfo`'s serde form and holds imports calls `rewind_scan_cursor(earliest import birth)` after each load; the import wrappers' API docs say so | Found while applying D81: the funds state lives in the same `ManagedCoreFundsAccount` as the pool (`managed_core_funds_account.rs:51-71`), so skipping the pool skips the funds. Core keeps transactions, not ownership, on disk; WL2a's schema already has `txs`/`utxos`. No consumer other than WL2a reaches the import API today |
| D83 | WL1 adds no persisted field to `ManagedWalletInfo`. Its serde form is byte-identical before and after WL1-5 in every format, and `managed_account_free.json` pins that. The rev6 claims of serde-default loading (§2.4, §2.5, D23) are withdrawn | Opus r4 m2: bincode-serde and `platform_value` cannot default missing fields (`managed_wallet_info/mod.rs:116-121`); the managed collection has the same struct-keyed maps as `AccountCollection`; Platform persists field by field. Derived accounts make all three moot |
| D84 | The seedless change rule sits in `ManagedWalletInfo::fund`, the one helper that calls `add_funding` → `fund_from` (shared by `funded_builder` and both asset-lock builders; it is what refuses `build_asset_lock_with_signer`, which has no root-key check). For a `Seedless` wallet without a caller change address it returns the existing `BuilderError::NoChangeAddress` before reserving. The three wallet-level build entry points gain a trailing `change_address: Option<Address>`. `next_change_address` keeps `Option` and returns `None` for `Seedless`, and the FFI change getter returns null with `InvalidState`. Tested through `build_unsigned_transaction` | Opus r4 m1: no builder calls `next_change_address`; change comes from the first funding account in `fund_from` (`transaction_builder.rs:234-236`), which knows no wallet type, so the guard goes one level up. Reusing `NoChangeAddress` avoids a variant in the exhaustive `BuilderError`. The added argument is Core's `changeAddress`, a 0.x source break with three call sites in this repository plus 13 key-wallet unit tests and none in Platform (§4.4, D92) |
| D85 | `Wallet::remove_imported(script) -> Result<bool, ImportError>` forgets an entry and its P2PK alias, wiping its secret. A sealed entry needs `Full`, since the kind-1 records digest is resealed (D61) with D62's stage-then-commit (D96); other entries can be removed in any state. `WalletManager::remove_imported` follows it with `sync_imported_pools` | The manager's r4 ruling names a remove-import test, and the WIF-sweep flow (§4.3) imports a key and later forgets it. Returning `bool` keeps the secret out of the caller's hands |
| D86 | `AccountSlot` has a manual `Debug` and a `Display` that print the slot kind and its numeric fields only. `TamperKind`'s `Display` uses them, and `Tampered`/`ForeignAccountKey` format `{0}` | Opus r4 n2: `{0:?}` printed two 32-byte identity ids for a DashPay slot; they are public, but §5.1 promises positions and counts only |
| D87 | The D73 recovery for a `SelfDerived` slot is to remove the account, unlock `Full`, and re-add it with `add_account(type, None)`. The test gets the original addresses back | Opus r4 n3: removal alone loses the user's account; the `None` path re-derives the correct xpub |
| D88 | The `const` item asserts `assert_codec::<EncryptedWallet>()` from WL1-4 on, plus `SealedRecord`/`MasterKeyRecord` if they get bincode impls | Opus r4 n4: the D80 rule named `EncryptedWallet`, but nothing forced its `BorrowDecode` to exist or to delegate |
| D89 | `account_free.json` and `managed_account_free.json` are generated by WL1-2's first commit on the WL1-1 head, before any slice changes a serde path | Opus r4 n5 and the manager's ruling: generating them in WL1-5 would bake WL1-2/WL1-4 output into "exactly the old JSON"; WL1-1 changed only the bincode codec, so its head still has pre-WL1 serde output. WL1-1 itself stays frozen for its gate |
| D90 | `from_wallet` seeds `synced_height` and `last_processed_height` at `min(birth_height, earliest import birth) − 1` (saturating; `Genesis` and unresolved `Time` → 0) after `sync_imported_pools`; `metadata.birth_height` stays the HD birth. Loaders that restore a persisted cursor overwrite it, and `insert_wallet` keeps a caller-built info's cursor | Opus r5 M1 and the manager's ruling: a wallet re-registered or restored through `from_wallet` monitored its imports but started at the HD birth, so `scan_start = max(birth, committed + 1)` skipped their history (`filters/manager.rs:212-240`). Core rescans from the first key time, which includes imported keys. The r4 test (HD birth 1000, import at 500, payment at 600) is restored |
| D91 | WL2a replays its `txs` records in block order with their stored contexts, then calls `update_balance`, before the wallet manager processes any block or mempool transaction and before any build or RPC | Opus r5 m1: the rebuilt scripts are monitored at once, so a block spending an imported coin before the replay is matched but never reaches `txs`; its `observed_spent_outpoints` entry is pruned once committed and chainlocked (`managed_wallet_info/mod.rs:84-95`), and the next load resurrects the coin. Core loads wallet transactions before attaching the wallet to the chain. The serde-only rewind path is order-safe |
| D92 | D84's facts corrected: no Platform package calls the three build entry points (the `rs-drive-abci` `build_unsigned_transaction` is a local three-argument function, and drive-abci does not use key-wallet); the callers are three in this repository plus 13 key-wallet unit tests. "A default would fail silently" stays with D78 only. The guard in `fund` is what refuses `build_asset_lock_with_signer`. Platform's own `add_funding` paths choose change without `fund` and must set a change address or refuse before Platform holds a `Seedless` wallet | Opus r5 m2 and the manager's ruling |
| D93 | `watch_only()` is a computed accessor over the `ImportedWatchOnly` account, not a `WalletCoreBalance` field; the cached `balance` is recomputed by the post-replay `update_balance` (D91). The serde-form test compares against the two fields set to `None` with `balance` untouched | Opus r5 n1: `balance` is persisted (`managed_wallet_info/mod.rs:58-59`), so a new field would break D83, and the cached total includes imported funds from before a reload until the replay |
| D94 | WL2a holds the wallet lock from the in-memory import through both writes | Opus r5 n2: another wallet-file write (a label, an account add) could persist the entry before the lowered cursor, leaving an entry without a rewind and a retry that is `AlreadyPresent`. Core holds `cs_wallet` across the same span |
| D95 | The D77 API docs, §4.1 and risk 5 name dash-spv's default anchor floor, `max(earliest_required_height, hd_wallet_sync_floor())` (`lifecycle.rs:113-120`), the HD activation height on mainnet, and note that imported births lower a fresh client's start height | Opus r5 n3: every mainnet import born before HD activation (the paper-wallet sweep of §4.3) is below the anchor by default; a `Genesis` import on testnet syncs headers from 0 |
| D96 | `remove_imported` of a sealed entry reseals kind-1 through D62's stage-then-commit; a failure leaves `self` unchanged and the secret is wiped only after the commit | Opus r5 n4: the reseal draws nonces and can fail with `Rng` |
| D97 | `ImportedScriptPool` implements `Serialize`/`Deserialize` and bincode `Encode`/`Decode`, because `ManagedAccountType` derives them; the two variants are appended | Opus r5 n6: "never serialised" refers to the `serde(skip)` collection fields, not to missing impls |

---

## 9. Open risks

1. **D3 and D14** (DashCore beyond imports; FR-WAL-15 versus unencrypted wallets) were accepted in Sol r1 and are treated as settled. They still refine PRD/A1 wording, so the manager should note them in DECISIONS when the briefs are cut.
2. **The FFI is not literally unchanged.** WL1-5 appends two `FFIAccountKind` values and arms at 8 key-wallet-ffi sites (§2.10); Sol r1 treats this as the settled ruling. Platform's exhaustive matches break on upgrade until they add arms (§4.4, D50). The remaining FFI gap is the consumer limit of D29.
3. **The hand-written `Wallet`/`WalletType` codec (WL1-1)** is a byte-exact rewrite of a derive. The golden tests guard it, but any field-order mistake corrupts existing iOS blobs. The goldens must be committed and passing against the derive **before** the swap.
4. **argon2 at 256 MiB on agentbox** under load (50–190) can take seconds. Tests stay on `TEST` params, and the single floor test is `#[ignore]` (§6.5).
5. **SPV rescans for `ImportBirth::Genesis`** are correct but slow for dash-spv consumers: one rescan from 0 per such import (D81). UIs should collect a birth height. On a checkpoint-synced client, and by default on mainnet below the HD activation floor (`lifecycle.rs:113-120`, D95), history below the header anchor is never scanned until the backlog row of §4.1 lands. The import wrappers' API docs say so (D77).
6. **Upstreaming (D21)** means the desktop keeps duplicating `bip39core`, `dw-message` and the vault primitives until a rust-dashcore release carries WL1. Track it in SW-DESK.
7. **WL4/WL5 will need record kinds 3+ and descriptor-imported accounts.** Under D30 they must bump the envelope or record version when they start writing them, and old readers will refuse those files.
8. **KDF policies across devices (D28, D44).** A mobile-written wallet (64 MiB) opens under `desktop()` and is re-wrapped at 256 MiB on the first unlock, so the caller must persist after `kdf_upgraded`. The other direction is unchanged: a desktop- or node-written wallet (256 MiB) does not open under `KdfPolicy::default()` until the caller raises the budget; consumers that exchange files must handle `KdfOverBudget`.
9. **Unlock check cost (D43).** Re-deriving every account and imported key on unlock is linear; a wallet with thousands of imported keys pays one AEAD open and one EC multiplication per key (milliseconds per thousand), small next to argon2 but worth a bench in WL1-6.
10. **D47 end state.** After a failed `upgrade_to_hd_encrypted` past the unlock, ours is always `Locked`; functional-test ports in WL2b assert that end state rather than transcribing Core's.
11. **Mempool gap for push-less scripts (D49).** Imported scripts with no data push are seen only at confirmation in SPV mode.
12. **Old MK after a passphrase change (D55)** remains usable with an old file copy and the old passphrase, as in Core. Documented in §5.1; no rekey in WL1.
13. **Account origin is coarse (D60, D72).** The origin and derivation path come from the slot, and a stored type or network that disagrees with the slot fails unlock and `encrypt`. An unencrypted wallet holding a foreign key in a self-derived slot cannot be encrypted until that account is removed or re-added; no current caller is known to do this (Platform's contact accounts are `External`).
14. **Source breaks (D78, D84).** Every `WalletInfoInterface`/`WalletInterface` implementor must add the required methods, and Platform needs three delegation arms; they have no defaults because a default would fail silently (D78). The three wallet-level build entry points gain a `change_address` argument, which touches three call sites in this repository plus 13 key-wallet unit tests; Platform has none (D92). A caller that keeps passing `None` for a `Seedless` wallet gets a loud `NoChangeAddress`. Both breaks are accepted under 0.x, like D50.
15. **Imported funds after a reload (D82).** The imported managed accounts are rebuilt without funds state. WL2a replays its transaction records. Any other consumer that holds imports and persists only `ManagedWalletInfo`'s serde form must rewind after each load, which costs a rescan from the earliest import birth per start. No such consumer exists today.

---

## 10. Rev1 changes (Sol r1, CHANGES-REQUIRED)

| Finding | Change |
|---|---|
| M1 encrypted `upgradetohd` path missing | New §2.6.1 `upgrade_to_hd_encrypted`: Core's order (preconditions, relock, unlock with the wallet passphrase, install, relock), reseal under the existing MK with a fresh nonce, one-step commit, failure atomicity, imported (kind-2) records byte-identical, ID preserved; `UpgradeError`; WL1-6 tests; D31. `upgrade_to_hd` returns `UpgradeError` and refuses encrypted wallets |
| M2 unknown-record preservation not representable | Preservation claim dropped: unknown version/kind/flag/trailing data → `UnsupportedFormat` for the whole file (§2.6 Versioning, §2.8 framing now exact); forward-compat rule and WL4/WL5 version bump; D30; risk 7 |
| M3 serde recipe incomplete | §2.4: `Wallet::imported` gets `default` + `skip_deserializing` + omit-when-empty + refuse-when-non-empty; `ImportedKeys` has no serde impls; old serde_json snapshots from the pre-swap commit, round-trip, refusal and injected-field tests (WL1-1 fixtures, WL1-5 tests) |
| M4 existing seed paths | New §2.4.1: signatures kept, zeroizing siblings added, public bare-copy accessors deprecated, internal callers (incl. `key-wallet-ffi/src/mnemonic.rs:209`) migrated in WL1-2, extraction gated on encrypted wallets in WL1-6, `Seed` stays `Copy`; zeroization claim scoped in §5.2 item 5; D27 |
| M5 decode gate | §2.8: WL1-1 is two commits; fixtures (bincode for all five variants, serde_json snapshots) from the old derive in the pre-swap commit; `new_encode == golden`, `new_decode(golden) == wallet` (field-wise helper), `restore`/`from_bytes` and the FFI `wallet_manager_import_wallet_from_bytes` path on the same bytes |
| M6 KDF read cap is not a DoS block | §2.6 pre-authentication `KdfBudget`, checked before argon2 allocates; mobile-safe default (64 MiB, t ≤ 24, p ≤ 4), `desktop()` policy, caller override up to the ceiling, `KdfOverBudget { required }` / `KdfAboveCeiling`; boundary tests in WL1-4; "blocks DoS" wording removed (§5.1, §5.2 item 8, D8); D28; risk 8 |
| M7 WL1-5 needs WL1-3 | WL1-5 depends on WL1-2 and WL1-3; §7 ordering sentence updated |
| m8 file-identity claim too broad | Header and D1 now list exactly which files and vector/assertion sections are identical at rc.3; `wallet_migration.py` (`test_conflict_txs`, `:667-719`) and `util_tests.cpp` outside the message tests named as differing |
| m9 mobile/FFI consumer limits | §0 item 7 and §2.10 state the limits (no passphrase constructor, no encryption/unlock or signing FFI, lock indistinguishable from `InvalidState`, no lock event); follow-up WL1-F; D29 |
| m10 DIP9 coverage overstated | §6.2 differential 1 compares BIP44 external, BIP44 internal and DIP9 external separately via `listdescriptors` + `deriveaddresses`; no Core internal DIP9 descriptor; legacy wallets via `getnewaddress`/`getrawchangeaddress` |
| m11 AAD wording | §2.6 and §5.2 item 7: AAD = header bytes before the nonce plus wallet ID; the nonce is authenticated as the AEAD nonce input; tamper matrix covers every byte incl. nonces (§6.3, WL1-4) |
| m12 imported-script differential too thin | §2.5 splits `import_address` / `import_raw_script(p2sh)` as Core does (`backup.cpp:287-307`); §6.2 differential 3 adds both `importaddress` forms, P2SH, the `p2sh`-with-address refusal and watch-only → spendable, compared through `getaddressinfo` |

### 10.1 Rev2 changes (Sol r2, CHANGES-REQUIRED)

| Finding | Change |
|---|---|
| M1 relock postcondition contradicts step order | §2.6.1 follows Core `wallet.cpp:373-430`: preconditions (AlreadyHd, PrivateKeysDisabled, NotEncrypted, EmptyPassphrase) first and lock state untouched on their failure; only calls past them relock → unlock → install → relock and end `Locked`. Postconditions and WL1-6 tests rewritten (unlocked/mixing-only/unencrypted stay so on precondition failure; wrong passphrase, over-budget and injected faults end `Locked`). D32; D31 reworded |
| M2 encrypted seedless has no kind-1 record | Kind-1 plaintext may be variant 6 with an empty payload; `SecretKind::Seedless`; decode checks and tests in §2.6; the upgrade swaps it for a fresh-nonce variant 0/5 record in one commit; blank → encrypt → upgrade round-trip test. D33 |
| M3 always-on `CryptoError` names a gated type | `KdfParams`/`KdfBudget` move to always-on `crypto/params.rs` (no argon2); WL1-2 bar adds `cargo check -p key-wallet --no-default-features`. D34 |
| M4 Zeroize derives do not compile | Derives dropped; manual `Zeroize` wipes only the private-key bytes, `Drop` on `ImportedKeys`, redacted manual `Debug`; tests for `{:?}` output and secret-only wiping (WL1-5). D35 |
| M5 wallet ID / root-key arms for new variants | `compute_wallet_id` returns the stored ID for `Seedless`/`Encrypted`; root-key accessors return typed errors and never `expect` (`wallet/mod.rs:153-161`, `root_extended_keys.rs:361-423` added to the exhaustive-match list); ID fixtures for create_seedless, import_from_bytes and encrypt. D36 |
| m1 KDF table says 256 MiB default | §3.2 KDF row and §5.1 disk-thief row distinguish `default()` (64 MiB, mobile) from `desktop()` (256 MiB) and state that a desktop file gives `KdfOverBudget` on a default reader until the caller opts in |

### 10.2 Rev3 changes (Sol r3, CHANGES-REQUIRED)

| Finding | Change |
|---|---|
| M1 payload traits | §2.3: `DashCoreMnemonic` gets a manual bincode codec (decode re-runs Core's Check) and a redacted `Debug`; `WalletMnemonic` derives `Debug`. §2.6: `EncryptedWallet` gets `Clone`, `Zeroize` (= lock) + `Drop`, a redacted `Debug` and an envelope-only codec; record types derive `Clone`/`PartialEq`. Trait-coverage section with a compile-time test across feature sets; `Box<EncryptedWallet>` reached through `DerefMut`. D37 |
| M2 `has_seed()` | §2.4: `has_seed` true for 0, 1, 5 and `Encrypted` with a mnemonic/seed kind, independent of lock state; default BLS/EdDSA accounts come from plaintext before sealing and never need an unlock; explicit seedless adds on a locked wallet give `InvalidState`; `has_seed` in the match inventory; predicate sweep (only `has_mnemonic`/`has_seed`/`can_sign` change, all made exhaustive); `bls,eddsa` default-account test for 0, 5 and encrypted-HD. D38 |
| m1 `Zeroizing` Debug rationale | §2.5 states zeroize 1.9.1 prints `Zeroizing { .. }`; the manual redacted `Debug` is kept as defence in depth. D39 |
| m2 locked-extraction summary | §2.6.1 "Existing paths": `wallet_seed_bytes` removed from the `InvalidState` list; seed extraction says it returns `None`, and `seed_zeroizing` `Err(Locked)`/`Err(MixingOnly)` |

### 10.3 Rev4 changes (Opus-h r1, CHANGES-REQUIRED)

| Finding | Change |
|---|---|
| Ruling: redact `Mnemonic` Debug | Premise corrected (bip39 2.2.2 prints only the language); redaction kept as hardening and extended to `RootExtendedPrivKey`; `{:?}` scan over every secret-bearing type, chain code counted as secret (§2.3, §5.2 item 1). D40 |
| M1 imported keys while encrypted | `ImportedKind::SealedPrivateKey { pubkey, record }` is the locked form; `imported` private with `pub(crate)` mutators; gated `Wallet::import_*`; seal on encrypt and on import while Full; lock leaves ciphertext only; `to_bytes` writes every sealed entry; load rules; on-demand signing; lifecycle tests (§2.5, §2.8). D41 |
| M2 stored IDs lost in `WalletManager` | In-memory `id_origin`, origin-aware `compute_wallet_id()`, all seven manager sites tested, re-register test, serde refuses `Stored`, legacy encoders refuse it (§2.4, §2.8). D42 |
| M3 public half unauthenticated | Unlock check over root, account, BLS/EdDSA and imported keys → `Tampered(TamperKind)`, stays Locked; records digest in the kind-1 AAD authenticates the kind-2 count and set; §5.1 names what is unauthenticated while locked; tamper tests (§2.6). D43 |
| M4 read-side KDF floor | Read floor removed (ceiling, budget, argon2 minimums only); floor on writes and migrate-on-unlock with `UnlockOutcome { kdf_upgraded }`; mobile→desktop open-and-upgrade tests, real-parameter test ignored (§2.6, §3.2, risk 8). D44 |
| m1 `WalletType` has no `Drop` | `impl Drop for WalletType` in WL1-1; explicit wipes in `lock`/`Drop`; §5.2 item 5 rationale corrected. D45 |
| m2 argon2 memory | `zeroize` feature, owned fallible buffer via `hash_password_into_with_memory`, wiped on every path, `OutOfMemory`; ceiling clamped to addressable memory (§2.6). D45 |
| m3 `Mnemonic` codec strings | Encode from `phrase_zeroizing()`, decode into `Zeroizing<String>`; codec added to the migrated list (§2.4.1). D45 |
| m4 lock state vs Core | Full + any passphrase → Ok; MixingOnly → MixingOnly re-verifies; `lock_to_mixing_only`; `change_passphrase` end states; per-case tests (§2.6, §3.1). D46 |
| m5 `upgradetohd` order | Phrase parsed after the unlock, Core's error precedence; every failure after the unlock ends Locked; error-order tests (§2.6.1). D47 |
| m6 P2PK not recognised | P2PK aliases on key/pubkey entries; P2PK case in differential 3 (§2.5, §6.2). D48 |
| m7 FFI inventory, Platform, non-additive variant | All 8 key-wallet-ffi sites with null pool getters (§2.10); Platform in §0 and new §4.4; `Encrypted` always compiled (D51); enums stay exhaustive. D50, D51 |
| m8 slice dependencies and gates | WL1-3/WL1-4 depend on WL1-2; WL1-4 no longer re-lists the always-on types; `require`/`lock_state` always compiled in `wallet/lock.rs` (§2.1, §2.6, §7). D52 |
| m9 test matrix and goldens | Lib-side `const` assertion and five `cargo check --lib` sets; goldens generated in key-wallet-ffi under `bincode,bls,eddsa` with BLS/EdDSA accounts (§2.6, §2.8, §6.5). D53 |
| m10 raw scripts and the mempool | `monitored_extra_scripts` + dash-spv push insertion; residual push-less gap documented (§2.5, §4.1). D49 |
| m11a kind-1 length leak | Kind-1 plaintext padded to 256-byte multiples, padding checked (§2.6). D54 |
| m11b MK kept on passphrase change | §5.1 row for the old-copy holder; no `rekey()` in WL1. D55 |
| m12 clones outlive `lock()` | Clones of an encrypted wallet are Locked; the manager's insert clone removed; test (§2.6). D56 |
| m13 `Display`/`Debug` claims | Claims scoped to WL1's new types plus the two redacted existing `Debug`s; existing `Display` impls named as outside the claim (§5.1, §5.2 item 1). D40 |
| m14 downgrade keeps imported keys | Downgrade converts private entries to watch-only pubkey entries, wipes secrets, drops records (§2.5 lifecycle, WL1-5/WL1-6). D57 |
| n1 guard length | "10 or more characters" (§1.2.1, §2.3) |
| n2 `root_xpub` type | `Option<RootExtendedPubKey>` (§2.6) |
| n3 `BorrowDecode` | `SecretBytes`'s codec gets `BorrowDecode` (§2.2) |
| n4 legacy tags 5–7 | The legacy decoder rejects tags 5–7 in every build (§2.4, WL1-1) |
| n5 weak-KDF gate | `insecure-test-kdf`, forwarded by nothing (§2.6, §5.2 item 4). D58 |
| n6 legacy trailing bytes | Legacy branch keeps today's leniency; envelope strict (§2.8). D59 |

### 10.4 Rev5 changes (Opus-h r2, CHANGES-REQUIRED)

| Finding | Change |
|---|---|
| M1 unlock check rejects legitimate wallets | Computed `AccountOrigin` (External / CallerSupplied / SelfDerived); unlock compares only self-derived accounts; §5.1 lists the other two as unauthenticated; account-add gates on `Encrypted`; `encrypt` (`ForeignAccountKey`) and upgrade (`CallerSuppliedAccounts`) preconditions; tests for the three legitimate cases and the self-derived swap (§2.6, §2.6.1). D60 |
| m1 kind-1 error mapping | Digest moved into the kind-1 plaintext; tag failure `Corrupt`, digest mismatch `Tampered(RecordSet)`; tests aligned (§2.6, §6.3). D61 |
| m2 import while Full not atomic | Staged kind-2 record, entry and kind-1 reseal committed together; RNG-fault test (§2.5). D62 |
| m3 padding bucket | Minimum 1024 bytes, size pass before allocation; D54 rationale corrected (§2.6). D63 |
| m4 dash-spv D49 path | Guard counts extra pushes, filter input and size, `all_accounts()` revision; raw-script-only, size and import-while-running tests (§2.5, §4.1). D64 |
| m5 history scan after import | Import birth recorded, `metadata.birth_height` lowered, #649 rewind and generation bump in WL1-5; synced-wallet test; out-of-range case as a backlog row owned by dash-spv sync (§2.5, §4.1, risk 5). D65 |
| m6 `was_locked` | `state == Locked` (§2.6). D66 |
| n1 `KeyUse` | `SecretUse` + `require(use_)` (§2.5). D67 |
| n2 downgrade listed as reseal | Removed; downgrade drops kind 1 (§2.6). D68 |
| n3 `unseal_all` | Dropped (§2.5). D68 |
| n4 FFI clone | §2.10 limit; D56 guarantee rests on `Clone` (§2.6). D69 |
| n5 managed side on downgrade | `WalletManager` wrapper resyncs; bare-method callers own it (§2.5). D70 |
| n6 argon2 features | `default-features = false, features = ["zeroize"]` (§2.6, WL1-4). D71 |

### 10.5 Rev6 changes (Opus-h r3, CHANGES-REQUIRED; WL1-1 rulings)

| Finding | Change |
|---|---|
| M1 origin from the stored type | `AccountSlot`; origin and derivation path from the slot; slot-consistency rule (type, network) at unlock and in the `encrypt` precondition; `account_origin(slot)`; relabel tests for a BIP44 and the operator-key slot; D60 rationale corrected (§2.6). D72 |
| m1 add gates advisory | `accounts` stays public; `TamperKind::AccountXpub`/`SpecialAccountKey` carry the slot; direct field writes outside the guarantee; §5.1 wording corrected (§2.6, §5.1). D73 |
| m2 §5.1 understated | External and CallerSupplied consequences named; seedless spends take an explicit change script and never use a CallerSupplied account; DashPay re-check as a Platform note (§2.5, §4.4, §5.1). D74 |
| m3 managed pools | §5.1 sentence; WL2a rebuilds or verifies pool scripts after unlock and at load (§4.2, §5.1). D75 |
| m4 import floor | `imports_floor` separate from the HD birth; load-time self-heal (§2.5). D76 |
| m5 backlog scope | Row narrowed to below-anchor; below-filter-start regtest variant; anchor limit in the wrappers' API docs (§2.5, §4.1, risk 5). D77 |
| m6 trait surface | Four required `WalletInfoInterface` methods and one required `WalletInterface` method; Platform's delegation arms in §4.4; risk 14. D78 |
| m7 / W1-3 serde snapshots | `.serde.bin` for the five variants (key-wallet-ffi); `account_free.json` for the injected-key, human-readable and old-JSON tests; WL1-1/WL1-5 rows and D18/D53 updated (§2.4). D79 |
| n1 stale AAD text | §2.6.1 steps 4–5 build and carry the digest in the plaintext (D61, D63) |
| n2 D43 opening clause | Re-derives self-derived slots only; slot consistency for all (D43) |
| n3 seedless accounts | "None by default" (§2.5) |
| n4 origin for keyless and imported kinds | `WatchOnly`/`ExternalSignable` slots are `CallerSupplied`; imported kinds have no slot (§2.6). D72 |
| n5 provider-key adds | Allowed in any lock state (§2.6) |
| n6 / W1-13 BorrowDecode | `const` item requires `BorrowDecode`; delegation rule for every secret type; `scan_script_pubkeys_for` named in §2.5 (§2.6). D80 |
| WL1-1 implementation | WL1-1 row marked implemented; §2.8 fixture-commit wording matches the landed order; `thiserror` moved to WL1-2; the `--lib` matrix has four sets until WL1-4 |

### 10.6 Rev7 changes (Opus-h r4, CHANGES-REQUIRED; manager rulings: simplify toward Core)

| Finding | Change |
|---|---|
| M1 self-heal misses orderings, no pool rebuild at load | `imports_floor` and the load-time self-heal dropped. The imported managed accounts are derived (`serde(skip)`) and reconciled by `sync_imported_pools` in `from_wallet`, at every insert, and after import, removal and downgrade. One scan cursor via `rewind_scan_cursor`; imported births reach `earliest_required_height` from the wallet. WL2a writes the cursor first, then the entry. New tests: crash between the writes, `from_wallet` restore, deserialised insert, removal, kind change (§2.5, §4.1, §4.2). D81 (supersedes D76), D85 |
| — imported funds state (found applying M1) | Skipping the pool skips its UTXOs: WL2a replays `txs`; serde-only consumers rewind after load (§2.5, §4.2, risk 15). D82 |
| m1 change guard in an unused function | The guard moves to `ManagedWalletInfo::fund` and returns `BuilderError::NoChangeAddress`. The three build entry points gain `change_address: Option<Address>`; `next_change_address` returns `None`; the FFI getter refuses; tests go through `build_unsigned_transaction` (§2.5, §4.4, §5.1, risk 14). D84 |
| m2 `ManagedWalletInfo` compatibility claims | No persisted managed field, so the serde form is unchanged; serde-default claims withdrawn; non-self-describing formats cited (`managed_wallet_info/mod.rs:116-121`); no Platform schema change; `managed_account_free.json` (§2.4, §2.5, §4.4, D23). D83 |
| n1 §2.9 error list | `ForeignAccountKey(AccountSlot)` and the slot payload of `TamperKind` named (§2.9) |
| n2 identity ids in errors | Slot `Display`/`Debug` print kind and numbers only; `{0}` in both messages; §5.1 row reworded; redaction test (§2.6). D86 |
| n3 D73 recovery loses the account | Re-add with `add_account(type, None)` while `Full`; the test gets the original addresses back (§2.6). D87 |
| n4 `EncryptedWallet` codec assertion | `assert_codec::<EncryptedWallet>()` from WL1-4 on (§2.6). D88 |
| n5 snapshot timing | Both JSON snapshots generated by WL1-2's first commit on the WL1-1 head (§2.4, WL1-2 row). D89 |
| Summary count | "71 D-rows" → 89; consumer item names the two source breaks |

### 10.7 Rev8 changes (Opus-h r5, CHANGES-REQUIRED; manager verifies the delta)

| Finding | Change |
|---|---|
| M1 `from_wallet` skips imports' history | `from_wallet` seeds the cursor at `min(HD birth, earliest import birth) − 1`; restoring loaders overwrite it; `insert_wallet` keeps a caller's cursor; the r4 test (HD birth 1000, import at 500, payment at 600) restored (§2.5, D81). D90 |
| m1 replay order | WL2a replays `txs` in block order, then `update_balance`, before any block, mempool or RPC processing (§2.5, §4.2, D82). D91 |
| m2 D84 facts | Platform caller claim dropped; real callers listed; "fail silently" kept for D78 only; the `fund` guard refuses `build_asset_lock_with_signer`; Platform's own `add_funding` paths noted (§2.5, §4.4, D84, risk 14). D92 |
| n1 persisted `balance` | `watch_only()` is a computed accessor; `balance` recomputed after the replay; serde test compares against `None` fields (§2.5). D93 |
| n2 lock across writes | WL2a holds the wallet lock through the import and both writes (§2.5, §4.2). D94 |
| n3 mainnet anchor floor | `hd_wallet_sync_floor()` named in the API docs, §4.1 and risk 5; imported births lower a fresh client's start height (§2.5, §4.1). D95 |
| n4 atomic removal | `remove_imported` reseals through D62's stage-then-commit (§2.5, D85). D96 |
| n5 change script vs address | D74 says `Address` |
| n6 pool impls | `ImportedScriptPool` gets serde and bincode impls; variants appended (§2.5). D97 |
