//! Shared fixtures for the `wallet_layout_v0` golden tests.
//!
//! The goldens in `key-wallet/tests/data/wallet_layout_v0/` hold one deterministic
//! wallet per `WalletType` variant, in two encodings:
//! - `<variant>.bin`: the bincode bytes (`bincode::config::standard()`, as
//!   `Wallet::backup` writes them). They were produced by the derived `Encode` before
//!   the hand-written codec replaced it, so they pin the on-disk layout that existing
//!   backups and FFI imports already use.
//! - `<variant>.serde.bin`: the same wallet through its serde derives
//!   (`bincode::serde`, which handles the struct-keyed account maps that serde_json
//!   refuses). Checked only in key-wallet-ffi, whose dev-dependencies enable
//!   bincode's `serde` feature.
//!
//! Every fixture carries BLS and EdDSA provider accounts, so the goldens depend on
//! `bls` and `eddsa`. `SHA256SUMS` pins every file.
//!
//! Two serde_json snapshots sit next to them (D89): `account_free.json`, a
//! variant-0 wallet with no DashPay or platform-payment account (serde_json cannot
//! encode those struct-keyed maps), and `managed_account_free.json`, its
//! `ManagedWalletInfo`. They were generated before WL1-2 changed any serde path, so
//! they pin the pre-WL1 human-readable serde output. Checked in key-wallet-ffi.
//!
//! Two envelope goldens sit next to them too: `<variant>.envelope.bin`, the
//! [`Wallet::to_bytes`] bytes (envelope v1) of a variant-5 wallet in each compat
//! mode, with the same account set as `mnemonic.bin`. `mnemonic_with_passphrase_bip39`
//! has a non-ASCII passphrase that NFKD changes; `mnemonic_with_passphrase_dash_core`
//! has a phrase only Dash Core's checksum accepts and a passphrase with a NUL byte.
//! Checked in both crates.
//!
//! Regenerate only from key-wallet-ffi: `WL1_REGEN_GOLDENS=1` writes missing files
//! and checks the existing ones; `WL1_REGEN_GOLDENS=force` overwrites them (only
//! when the layout is meant to change).
//!
//! Included by path from `key-wallet/tests/wallet_layout_v0.rs` and
//! `key-wallet-ffi/tests/wallet_layout_v0.rs`.

#![allow(dead_code)]

use std::path::PathBuf;

use key_wallet::account::{AccountType, StandardAccountType};
use key_wallet::dashcore::hashes::{sha256, Hash};
use key_wallet::mnemonic::{MnemonicCompat, WalletMnemonic};
use key_wallet::wallet::initialization::WalletAccountCreationOptions;
use key_wallet::wallet::root_extended_keys::RootExtendedPrivKey;
use key_wallet::wallet::{ManagedWalletInfo, WalletType};
use key_wallet::{ExtendedPrivKey, ExtendedPubKey, Mnemonic, Network, Passphrase, Seed, Wallet};

const REGEN_ENV: &str = "WL1_REGEN_GOLDENS";

pub const VARIANTS: [&str; 5] =
    ["mnemonic", "seed", "extended_priv_key", "external_signable", "watch_only"];

/// Variant-5 fixtures pinned as `<variant>.envelope.bin`.
pub const ENVELOPE_VARIANTS: [&str; 2] =
    ["mnemonic_with_passphrase_bip39", "mnemonic_with_passphrase_dash_core"];

const NETWORK: Network = Network::Testnet;
const PHRASE: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
const SEED_WALLET: [u8; 64] = fill(0x11);
const SEED_XPRIV_MASTER: [u8; 64] = fill(0x22);
const SEED_XPRIV_PROVIDER: [u8; 64] = fill(0x33);
const SEED_XPUB_MASTER: [u8; 64] = fill(0x44);
const SEED_CONTACT: [u8; 64] = fill(0x55);
/// NFKD turns the `Å` (U+00C5) into `A` + U+030A, so the seed hashes other bytes than
/// the file stores.
const BIP39_PASSPHRASE: &str = "TREZOR-\u{c5}";
/// Dash Core's checksum accepts it, strict BIP39's does not (desktop quirk corpus).
const CORE_ONLY_PHRASE: &str =
    "alter undo step harbor skate color young picture sand chef goat ordinary";
const DASH_CORE_PASSPHRASE: &[u8] = b"custom-passphrase\0wl1";

const fn fill(start: u8) -> [u8; 64] {
    let mut out = [0u8; 64];
    let mut i = 0;
    while i < 64 {
        out[i] = start.wrapping_add(i as u8);
        i += 1;
    }
    out
}

fn data_dir() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../key-wallet/tests/data/wallet_layout_v0"))
}

fn sums_path() -> PathBuf {
    data_dir().join("SHA256SUMS")
}

fn read(name: &str) -> Vec<u8> {
    let path = data_dir().join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

pub fn read_golden(variant: &str) -> Vec<u8> {
    read(&format!("{variant}.bin"))
}

pub fn read_envelope_golden(variant: &str) -> Vec<u8> {
    read(&format!("{variant}.envelope.bin"))
}

#[derive(Clone, Copy, PartialEq)]
enum Regen {
    Off,
    Missing,
    Force,
}

/// Regeneration mode. Only key-wallet-ffi may regenerate (the goldens are its
/// default-feature bytes); the key-wallet copy of this file always checks.
fn regen_mode() -> Regen {
    match std::env::var(REGEN_ENV).as_deref() {
        Err(_) => Regen::Off,
        Ok(_) if env!("CARGO_PKG_NAME") != "key-wallet-ffi" => {
            panic!("{REGEN_ENV}: regenerate from key-wallet-ffi only")
        }
        Ok("force") => Regen::Force,
        Ok(_) => Regen::Missing,
    }
}

pub fn regen() -> bool {
    regen_mode() != Regen::Off
}

fn encode(wallet: &Wallet) -> Vec<u8> {
    bincode::encode_to_vec(wallet, bincode::config::standard()).expect("encode wallet")
}

/// Decodes `bytes` and requires that the whole buffer is consumed.
fn decode(bytes: &[u8]) -> Wallet {
    let (wallet, read): (Wallet, usize) =
        bincode::decode_from_slice(bytes, bincode::config::standard()).expect("decode wallet");
    assert_eq!(read, bytes.len(), "trailing bytes after the wallet");
    wallet
}

/// A second encoding of the fixtures, pinned as `<variant>.<suffix>.bin`.
pub struct Snapshot {
    pub suffix: &'static str,
    pub encode: fn(&Wallet) -> Vec<u8>,
    pub decode: fn(&[u8]) -> Wallet,
}

/// Builds the fixture wallet for `variant`.
pub fn fixture(variant: &str) -> Wallet {
    match variant {
        "mnemonic" => mnemonic_wallet(),
        "seed" => seed_wallet(),
        "extended_priv_key" => extended_priv_key_wallet(),
        "external_signable" => xpub_wallet(true),
        "watch_only" => xpub_wallet(false),
        other => panic!("unknown variant {other}"),
    }
}

/// Builds the envelope fixture wallet for `variant`.
pub fn envelope_fixture(variant: &str) -> Wallet {
    let (phrase, passphrase, compat) = match variant {
        "mnemonic_with_passphrase_bip39" => {
            (PHRASE, BIP39_PASSPHRASE.as_bytes(), MnemonicCompat::Bip39)
        }
        "mnemonic_with_passphrase_dash_core" => {
            (CORE_ONLY_PHRASE, DASH_CORE_PASSPHRASE, MnemonicCompat::DashCore)
        }
        other => panic!("unknown envelope variant {other}"),
    };
    let mut wallet = Wallet::from_phrase(
        phrase,
        Passphrase::from_slice(passphrase),
        compat,
        NETWORK,
        WalletAccountCreationOptions::Default,
    )
    .expect("passphrase wallet");
    add_extra_accounts(&mut wallet);
    wallet
}

/// Default accounts (BLS/EdDSA come from the mnemonic seed), plus a second BIP44
/// account and a DashPay contact account with a foreign xpub.
fn mnemonic_wallet() -> Wallet {
    let mnemonic = Mnemonic::from_phrase(PHRASE).expect("phrase");
    let mut wallet =
        Wallet::from_mnemonic(mnemonic, NETWORK, WalletAccountCreationOptions::Default)
            .expect("mnemonic wallet");
    add_extra_accounts(&mut wallet);
    wallet
}

/// A second BIP44 account and a DashPay contact account with a foreign xpub.
fn add_extra_accounts(wallet: &mut Wallet) {
    wallet
        .add_account(
            AccountType::Standard {
                index: 1,
                standard_account_type: StandardAccountType::BIP44Account,
            },
            None,
        )
        .expect("bip44 #1");
    let contact = ExtendedPubKey::from_priv(
        &ExtendedPrivKey::new_master(NETWORK, &SEED_CONTACT).expect("contact master"),
    );
    wallet
        .add_account(
            AccountType::DashpayExternalAccount {
                index: 0,
                user_identity_id: [0xA1; 32],
                friend_identity_id: [0xB2; 32],
            },
            Some(contact),
        )
        .expect("dashpay external");
}

fn seed_wallet() -> Wallet {
    Wallet::from_seed(Seed::new(SEED_WALLET), NETWORK, WalletAccountCreationOptions::Default)
        .expect("seed wallet")
}

/// An xpriv-built wallet has no seed, so its BLS/EdDSA accounts come from a
/// separately supplied seed.
fn extended_priv_key_wallet() -> Wallet {
    let master = ExtendedPrivKey::new_master(NETWORK, &SEED_XPRIV_MASTER).expect("master");
    let mut wallet = Wallet::from_extended_key(master, WalletAccountCreationOptions::Default)
        .expect("xpriv wallet");
    wallet
        .add_bls_account(AccountType::ProviderOperatorKeys, Some(&SEED_XPRIV_PROVIDER))
        .expect("bls");
    wallet
        .add_eddsa_account(AccountType::ProviderPlatformKeys, Some(&SEED_XPRIV_PROVIDER))
        .expect("eddsa");
    wallet
}

/// Keyless wallets carry the seed wallet's account collection (BLS/EdDSA included),
/// so their accounts name the seed wallet as parent, not this wallet's id. That is
/// fine for a layout pin; `watch_only` and `external_signable` differ only in the tag.
fn xpub_wallet(can_sign_externally: bool) -> Wallet {
    let master = ExtendedPrivKey::new_master(NETWORK, &SEED_XPUB_MASTER).expect("master");
    let master_xpub = ExtendedPubKey::from_priv(&master);
    Wallet::from_xpub(master_xpub, seed_wallet().accounts.clone(), can_sign_externally)
        .expect("xpub wallet")
}

/// Variant name of a wallet type, matching `VARIANTS`.
fn variant_name(wallet_type: &WalletType) -> &'static str {
    match wallet_type {
        WalletType::Mnemonic {
            ..
        } => "mnemonic",
        WalletType::Seed {
            ..
        } => "seed",
        WalletType::ExtendedPrivKey(_) => "extended_priv_key",
        WalletType::ExternalSignable => "external_signable",
        WalletType::WatchOnly => "watch_only",
        WalletType::MnemonicWithPassphrase {
            ..
        } => "mnemonic_with_passphrase",
    }
}

/// Field-by-field equality: `Wallet` has no `PartialEq`. Accounts are compared via
/// their `Debug` form, which does not depend on the codec under test.
pub fn assert_wallet_eq(actual: &Wallet, expected: &Wallet) {
    assert_eq!(actual.network, expected.network, "network");
    assert_eq!(actual.wallet_id, expected.wallet_id, "wallet_id");
    match (&actual.wallet_type, &expected.wallet_type) {
        (
            WalletType::Mnemonic {
                mnemonic: a,
                root_extended_private_key: ar,
            },
            WalletType::Mnemonic {
                mnemonic: e,
                root_extended_private_key: er,
            },
        ) => {
            assert_eq!(a.phrase_zeroizing(), e.phrase_zeroizing(), "mnemonic");
            assert_root_eq(ar, er);
        }
        (
            WalletType::Seed {
                seed: a,
                root_extended_private_key: ar,
            },
            WalletType::Seed {
                seed: e,
                root_extended_private_key: er,
            },
        ) => {
            assert_eq!(a.as_slice(), e.as_slice(), "seed");
            assert_root_eq(ar, er);
        }
        (
            WalletType::MnemonicWithPassphrase {
                mnemonic: a,
                passphrase: ap,
                root_extended_private_key: ar,
            },
            WalletType::MnemonicWithPassphrase {
                mnemonic: e,
                passphrase: ep,
                root_extended_private_key: er,
            },
        ) => {
            assert_eq!(a.compat(), e.compat(), "compat");
            assert_eq!(mnemonic_bytes(a), mnemonic_bytes(e), "mnemonic");
            assert_eq!(ap, ep, "passphrase");
            assert_root_eq(ar, er);
        }
        (WalletType::ExtendedPrivKey(a), WalletType::ExtendedPrivKey(e)) => assert_root_eq(a, e),
        (WalletType::ExternalSignable, WalletType::ExternalSignable)
        | (WalletType::WatchOnly, WalletType::WatchOnly) => {}
        (a, e) => panic!("variant {} != {}", variant_name(a), variant_name(e)),
    }
    assert_eq!(format!("{:?}", actual.accounts), format!("{:?}", expected.accounts), "accounts");
}

fn mnemonic_bytes(mnemonic: &WalletMnemonic) -> Vec<u8> {
    match mnemonic {
        WalletMnemonic::Bip39(m) => m.phrase_zeroizing().as_bytes().to_vec(),
        WalletMnemonic::DashCore(m) => m.phrase().expose().to_vec(),
    }
}

fn assert_root_eq(actual: &RootExtendedPrivKey, expected: &RootExtendedPrivKey) {
    assert_eq!(
        actual.root_private_key.to_secret_bytes(),
        expected.root_private_key.to_secret_bytes(),
        "root private key"
    );
    assert_eq!(actual.root_chain_code, expected.root_chain_code, "root chain code");
}

/// Byte equality that reports the first differing offset.
fn assert_bytes_eq(actual: &[u8], expected: &[u8], what: &str) {
    if let Some(at) = actual.iter().zip(expected).position(|(a, e)| a != e) {
        panic!("{what}: first difference at byte {at} of {}", expected.len());
    }
    assert_eq!(actual.len(), expected.len(), "{what}: length");
}

/// Checks one golden file against `encoded` (the fixture's encoding), writing it
/// first when regenerating and it is missing (or always, with `force`).
fn check_file(name: &str, encoded: &[u8], mode: Regen) -> Vec<u8> {
    let path = data_dir().join(name);
    if mode == Regen::Force || (mode == Regen::Missing && !path.exists()) {
        std::fs::create_dir_all(data_dir()).expect("create data dir");
        std::fs::write(&path, encoded).expect("write golden");
    }
    let golden = read(name);
    assert_bytes_eq(encoded, &golden, &format!("{name}: encode(fixture) vs golden"));
    golden
}

/// For every fixture: `encode(fixture) == golden`, `decode(golden) == fixture`,
/// `encode(decode(golden)) == golden`, `Wallet::restore(golden) == fixture`,
/// `Wallet::from_bytes(golden) == fixture` and `fixture.to_bytes() == golden`; the
/// same for `snapshot` when given; and `SHA256SUMS` against every file it lists
/// (rewritten from the files on disk when regenerating).
pub fn check_all_goldens(snapshot: Option<Snapshot>) {
    let mode = regen_mode();
    assert!(mode == Regen::Off || snapshot.is_some(), "regenerate with the serde snapshot");
    let mut names = Vec::new();
    for variant in VARIANTS {
        let wallet = fixture(variant);
        assert_eq!(variant_name(&wallet.wallet_type), variant);
        assert!(wallet.accounts.provider_operator_keys.is_some(), "{variant}: BLS account");
        assert!(wallet.accounts.provider_platform_keys.is_some(), "{variant}: EdDSA account");

        let name = format!("{variant}.bin");
        let golden = check_file(&name, &encode(&wallet), mode);
        let decoded = decode(&golden);
        assert_wallet_eq(&decoded, &wallet);
        assert_bytes_eq(&encode(&decoded), &golden, &format!("{name}: re-encode"));
        assert_wallet_eq(&Wallet::restore(&golden).expect("restore"), &wallet);
        assert_wallet_eq(&Wallet::from_bytes(&golden).expect("from_bytes"), &wallet);
        assert_bytes_eq(
            &wallet.to_bytes().expect("to_bytes"),
            &golden,
            &format!("{name}: to_bytes"),
        );
        names.push(name);

        if let Some(snapshot) = &snapshot {
            let name = format!("{variant}.{}.bin", snapshot.suffix);
            let golden = check_file(&name, &(snapshot.encode)(&wallet), mode);
            let decoded = (snapshot.decode)(&golden);
            assert_wallet_eq(&decoded, &wallet);
            assert_bytes_eq(&(snapshot.encode)(&decoded), &golden, &format!("{name}: re-encode"));
            names.push(name);
        }
    }

    check_sums(&names, mode);
}

/// For every envelope fixture: `fixture.to_bytes() == golden`,
/// `Wallet::from_bytes(golden) == fixture`, `from_bytes(golden).to_bytes() == golden`
/// and the legacy reader refuses the golden; then `SHA256SUMS`. Regeneration follows
/// the same rules as [`check_all_goldens`].
pub fn check_envelope_goldens() {
    let mode = regen_mode();
    let mut names = Vec::new();
    for variant in ENVELOPE_VARIANTS {
        let wallet = envelope_fixture(variant);
        assert_eq!(variant_name(&wallet.wallet_type), "mnemonic_with_passphrase");
        assert!(wallet.accounts.provider_operator_keys.is_some(), "{variant}: BLS account");
        assert!(wallet.accounts.provider_platform_keys.is_some(), "{variant}: EdDSA account");

        let name = format!("{variant}.envelope.bin");
        let golden = check_file(&name, &wallet.to_bytes().expect("to_bytes"), mode);
        assert_eq!(golden[..5], [0xFF, b'K', b'W', b'W', 1], "{name}: envelope v1 header");
        let decoded = Wallet::from_bytes(&golden).expect("from_bytes");
        assert_wallet_eq(&decoded, &wallet);
        assert_bytes_eq(
            &decoded.to_bytes().expect("to_bytes"),
            &golden,
            &format!("{name}: re-encode"),
        );
        assert!(Wallet::restore(&golden).is_err(), "{name}: the legacy reader accepted it");
        names.push(name);
    }
    check_sums(&names, mode);
}

fn sums_line(name: &str) -> String {
    format!("{}  {name}\n", sha256::Hash::hash(&read(name)))
}

/// Verifies `SHA256SUMS` against every file it lists and requires that each of
/// `names` is listed. When regenerating, the lines for `names` are rewritten from
/// the files on disk first; lines for files checked elsewhere are kept.
fn check_sums(names: &[String], mode: Regen) {
    if mode != Regen::Off {
        let existing = std::fs::read_to_string(sums_path()).unwrap_or_default();
        let mut lines: Vec<(String, String)> = existing
            .lines()
            .filter_map(|line| line.split_once("  "))
            .filter(|(_, name)| !names.iter().any(|n| n == name))
            .map(|(_, name)| (name.to_string(), sums_line(name)))
            .collect();
        lines.extend(names.iter().map(|name| (name.clone(), sums_line(name))));
        lines.sort();
        let sums: String = lines.into_iter().map(|(_, line)| line).collect();
        std::fs::write(sums_path(), sums).expect("write SHA256SUMS");
    }
    let pinned = String::from_utf8(read("SHA256SUMS")).expect("SHA256SUMS is UTF-8");
    for line in pinned.lines() {
        let (_, name) = line.split_once("  ").expect("sha256sum format");
        assert_eq!(format!("{line}\n"), sums_line(name), "SHA256SUMS: {name}");
    }
    for name in names {
        assert!(pinned.lines().any(|line| line.ends_with(&format!("  {name}"))), "unpinned {name}");
    }
}

/// The serde_json snapshot wallet (D89): a variant-0 wallet with BIP44, BIP32,
/// CoinJoin and identity accounts only. serde_json cannot encode the struct-keyed
/// DashPay and platform-payment maps once they hold an account, so this wallet has
/// none; it pins the human-readable serde path that the `.serde.bin` goldens miss.
pub fn account_free_wallet() -> Wallet {
    let options = WalletAccountCreationOptions::SpecificAccounts(
        [0, 1].into(),
        [0].into(),
        [0].into(),
        [0].into(),
        Default::default(),
        Some(vec![
            AccountType::IdentityRegistration,
            AccountType::IdentityTopUpNotBoundToIdentity,
            AccountType::IdentityInvitation,
        ]),
    );
    let mnemonic = Mnemonic::from_phrase(PHRASE).expect("phrase");
    Wallet::from_mnemonic(mnemonic, NETWORK, options).expect("account-free wallet")
}

/// Birth height of the `managed_account_free.json` snapshot.
pub const ACCOUNT_FREE_BIRTH_HEIGHT: u32 = 1_000;

/// The managed info of [`account_free_wallet`], as `WalletManager` builds it.
pub fn account_free_info(wallet: &Wallet) -> ManagedWalletInfo {
    ManagedWalletInfo::from_wallet(wallet, ACCOUNT_FREE_BIRTH_HEIGHT)
}

/// Maps a committed snapshot to what a build with different features writes for it.
pub type SnapshotView = fn(&[u8]) -> Vec<u8>;

/// One serde_json snapshot: how to encode and decode the pinned value.
pub struct JsonSnapshot<T> {
    pub name: &'static str,
    pub encode: fn(&T) -> Vec<u8>,
    pub decode: fn(&[u8]) -> T,
    /// What this build writes for the committed snapshot, when a feature of the build
    /// changes the layout (`None`: the snapshot itself). The committed file is always the
    /// default-feature layout, so regenerating needs a build where this is `None`.
    pub expected: Option<SnapshotView>,
}

impl<T> JsonSnapshot<T> {
    /// The committed snapshot and what this build must encode the fixture to.
    fn golden(&self, fixture: &T, mode: Regen) -> (Vec<u8>, Vec<u8>) {
        let encoded = (self.encode)(fixture);
        match self.expected {
            None => {
                let golden = check_file(self.name, &encoded, mode);
                (golden.clone(), golden)
            }
            Some(view) => {
                assert!(mode == Regen::Off, "{}: regenerate in a default-feature build", self.name);
                let golden = read(self.name);
                let expected = view(&golden);
                assert_bytes_eq(
                    &encoded,
                    &expected,
                    &format!("{}: encode(fixture) vs golden", self.name),
                );
                (golden, expected)
            }
        }
    }
}

/// For each snapshot: `encode(fixture) == golden`, `encode(decode(golden)) ==
/// golden`, and `check(decode(golden), fixture)`, with `golden` as this build writes
/// it ([`JsonSnapshot::expected`]); then `SHA256SUMS`. Regeneration follows the same
/// rules as [`check_all_goldens`].
pub fn check_json_snapshots(wallet: JsonSnapshot<Wallet>, info: JsonSnapshot<ManagedWalletInfo>) {
    let mode = regen_mode();
    let fixture = account_free_wallet();
    assert!(fixture.accounts.dashpay_receival_accounts.is_empty());
    assert!(fixture.accounts.dashpay_external_accounts.is_empty());
    assert!(fixture.accounts.platform_payment_accounts.is_empty());

    let (golden, expected) = wallet.golden(&fixture, mode);
    let decoded = (wallet.decode)(&golden);
    assert_wallet_eq(&decoded, &fixture);
    assert_bytes_eq(&(wallet.encode)(&decoded), &expected, &format!("{}: re-encode", wallet.name));

    let fixture_info = account_free_info(&fixture);
    let (golden, expected) = info.golden(&fixture_info, mode);
    let decoded = (info.decode)(&golden);
    assert_info_eq(&decoded, &fixture_info);
    assert_bytes_eq(&(info.encode)(&decoded), &expected, &format!("{}: re-encode", info.name));

    check_sums(&[wallet.name.to_string(), info.name.to_string()], mode);
}

/// `ManagedWalletInfo` has no `PartialEq`, and its `Debug` walks hash maps, so the
/// accounts are compared through their addresses per pool.
fn assert_info_eq(actual: &ManagedWalletInfo, expected: &ManagedWalletInfo) {
    assert_eq!(actual.network, expected.network, "network");
    assert_eq!(actual.wallet_id, expected.wallet_id, "wallet_id");
    assert_eq!(actual.name, expected.name, "name");
    assert_eq!(actual.description, expected.description, "description");
    assert_eq!(format!("{:?}", actual.metadata), format!("{:?}", expected.metadata), "metadata");
    assert_eq!(actual.balance, expected.balance, "balance");
    let addresses = |info: &ManagedWalletInfo| -> Vec<Vec<String>> {
        info.accounts
            .all_accounts()
            .into_iter()
            .map(|account| {
                account.all_addresses().iter().map(ToString::to_string).collect::<Vec<_>>()
            })
            .collect()
    };
    assert_eq!(actual.accounts.all_accounts().len(), expected.accounts.all_accounts().len());
    assert_eq!(addresses(actual), addresses(expected), "account addresses");
}
