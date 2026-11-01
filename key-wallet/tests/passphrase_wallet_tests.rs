//! Passphrase and Dash Core-mode wallets (variant 5): default accounts (D38) and the mnemonic
//! codecs (D37).

use std::collections::BTreeSet;

use hex_conservative::DisplayHex;

use key_wallet::account::AccountType;
use key_wallet::crypto::CryptoError;
use key_wallet::mnemonic::{core_seed, DashCoreMnemonic, MnemonicCompat, WalletMnemonic};
use key_wallet::wallet::initialization::WalletAccountCreationOptions;
use key_wallet::wallet::WalletType;
use key_wallet::{ExtendedPrivKey, Network, Passphrase, SecretBytes, Wallet};

const STRICT: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
/// Core accepts its checksum, strict BIP39 does not (from the desktop quirk corpus).
const CORE_ONLY: &str = "alter undo step harbor skate color young picture sand chef goat ordinary";
/// Core rejects its checksum (same corpus, "12 words, checksum 0000").
const CORE_REJECTS: &str =
    "alter undo step harbor skate color young picture sand chef goat orchard";

fn wallet(phrase: &str, passphrase: &[u8], compat: MnemonicCompat) -> Wallet {
    Wallet::from_phrase(
        phrase,
        Passphrase::from_slice(passphrase),
        compat,
        Network::Testnet,
        WalletAccountCreationOptions::Default,
    )
    .unwrap()
}

fn operator_and_platform_keys(wallet: &Wallet) -> (String, String) {
    let accounts = &wallet.accounts;
    let bls = accounts.provider_operator_keys.as_ref().expect("BLS operator account");
    let eddsa = accounts.provider_platform_keys.as_ref().expect("EdDSA platform account");
    (format!("{:?}", bls.bls_public_key), format!("{:?}", eddsa.ed25519_public_key))
}

/// Every account type the wallet holds: the ECDSA accounts plus the BLS and EdDSA ones.
fn account_types(wallet: &Wallet) -> BTreeSet<AccountType> {
    let accounts = &wallet.accounts;
    let mut types: BTreeSet<AccountType> =
        accounts.all_accounts().into_iter().map(|a| a.account_type).collect();
    types.extend(accounts.provider_operator_keys.as_ref().map(|a| a.account_type));
    types.extend(accounts.provider_platform_keys.as_ref().map(|a| a.account_type));
    types
}

/// `has_seed` holds for variant 5, so default creation adds the BLS operator and EdDSA platform
/// accounts for passphrase and DashCore wallets as it does for variant 0 (D38). A DashCore
/// wallet of a strict phrase with an empty passphrase has variant 0's seed, so the same keys.
#[test]
fn default_accounts_include_bls_and_eddsa_for_variant_5() {
    let plain = wallet(STRICT, b"", MnemonicCompat::Bip39);
    assert!(matches!(plain.wallet_type, WalletType::Mnemonic { .. }));
    assert!(plain.has_seed());
    let plain_keys = operator_and_platform_keys(&plain);
    let plain_types = account_types(&plain);
    assert!(plain_types.contains(&AccountType::ProviderOperatorKeys));
    assert!(plain_types.contains(&AccountType::ProviderPlatformKeys));

    let same_seed = wallet(STRICT, b"", MnemonicCompat::DashCore);
    assert!(matches!(same_seed.wallet_type, WalletType::MnemonicWithPassphrase { .. }));
    assert_eq!(operator_and_platform_keys(&same_seed), plain_keys);
    assert_eq!(account_types(&same_seed), plain_types);
    assert_eq!(same_seed.wallet_id, plain.wallet_id);
    assert_eq!(
        same_seed.accounts.standard_bip44_accounts[&0].account_xpub,
        plain.accounts.standard_bip44_accounts[&0].account_xpub
    );

    for (phrase, passphrase, compat) in [
        (STRICT, &b"TREZOR"[..], MnemonicCompat::Bip39),
        (STRICT, &b"TREZOR\0"[..], MnemonicCompat::DashCore),
        (CORE_ONLY, &b""[..], MnemonicCompat::DashCore),
    ] {
        let other = wallet(phrase, passphrase, compat);
        assert!(matches!(other.wallet_type, WalletType::MnemonicWithPassphrase { .. }));
        assert!(other.has_seed() && other.has_mnemonic() && other.can_sign());
        assert_eq!(account_types(&other), plain_types);
        let keys = operator_and_platform_keys(&other);
        assert_ne!(keys.0, plain_keys.0);
        assert_ne!(keys.1, plain_keys.1);
    }
}

#[test]
fn dash_core_mnemonic_codec() {
    let config = bincode::config::standard();
    let core_only = DashCoreMnemonic::parse(CORE_ONLY.as_bytes()).unwrap();
    assert!(!core_only.is_strict_bip39());
    let bytes = bincode::encode_to_vec(&core_only, config).unwrap();
    // The layout of `SecretBytes` (and of `String`): a length prefix, then the bytes.
    assert_eq!(bytes, bincode::encode_to_vec(CORE_ONLY, config).unwrap());
    let (back, read): (DashCoreMnemonic, usize) =
        bincode::decode_from_slice(&bytes, config).unwrap();
    assert_eq!((back, read), (core_only, bytes.len()));
    let (borrowed, _): (DashCoreMnemonic, usize) =
        bincode::borrow_decode_from_slice(&bytes, config).unwrap();
    assert_eq!(borrowed.phrase().expose(), CORE_ONLY.as_bytes());

    // A phrase Core's Check refuses does not decode, and the error names no word.
    let rejected =
        bincode::encode_to_vec(SecretBytes::from_slice(CORE_REJECTS.as_bytes()), config).unwrap();
    let err = bincode::decode_from_slice::<DashCoreMnemonic, _>(&rejected, config).unwrap_err();
    let text = format!("{err} {err:?}");
    assert!(text.contains("invalid Dash Core mnemonic"), "{text}");
    for word in CORE_REJECTS.split(' ') {
        assert!(!text.contains(word), "{text}");
    }
}

#[test]
fn wallet_mnemonic_codec_round_trips() {
    let config = bincode::config::standard();
    for (phrase, compat) in [
        (STRICT, MnemonicCompat::Bip39),
        (STRICT, MnemonicCompat::DashCore),
        (CORE_ONLY, MnemonicCompat::DashCore),
    ] {
        let mnemonic = WalletMnemonic::parse(phrase, compat).unwrap();
        let bytes = bincode::encode_to_vec(&mnemonic, config).unwrap();
        let (back, _): (WalletMnemonic, usize) =
            bincode::decode_from_slice(&bytes, config).unwrap();
        assert_eq!(back.compat(), compat);
        let pass = Passphrase::from_slice(b"p");
        assert_eq!(*back.to_seed(&pass).unwrap(), *mnemonic.to_seed(&pass).unwrap());
        assert_eq!(bincode::encode_to_vec(&back, config).unwrap(), bytes);
    }
}

/// `WalletMnemonic`'s tags are fixed: 0 is BIP39, 1 is Dash Core; any other tag is refused.
#[test]
fn wallet_mnemonic_tags_are_pinned() {
    let config = bincode::config::standard();
    for (compat, tag) in [(MnemonicCompat::Bip39, 0u8), (MnemonicCompat::DashCore, 1)] {
        let bytes =
            bincode::encode_to_vec(WalletMnemonic::parse(STRICT, compat).unwrap(), config).unwrap();
        assert_eq!(bytes[0], tag, "{compat:?}");
        // The tag, then the phrase as a string.
        assert_eq!(bytes[1..], bincode::encode_to_vec(STRICT, config).unwrap());
    }
    let mut bytes = bincode::encode_to_vec(
        WalletMnemonic::parse(STRICT, MnemonicCompat::Bip39).unwrap(),
        config,
    )
    .unwrap();
    bytes[0] = 2;
    let err = bincode::decode_from_slice::<WalletMnemonic, _>(&bytes, config).unwrap_err();
    assert!(
        matches!(
            err,
            bincode::error::DecodeError::UnexpectedVariant {
                type_name: "WalletMnemonic",
                found: 2,
                ..
            }
        ),
        "{err:?}"
    );
}

/// D109: only a non-empty passphrase is reported; Dash Core mode with an empty one is variant 5
/// but has no passphrase to show.
#[test]
fn mnemonic_passphrase_reports_only_a_non_empty_one() {
    let core_empty = wallet(STRICT, b"", MnemonicCompat::DashCore);
    assert!(matches!(core_empty.wallet_type, WalletType::MnemonicWithPassphrase { .. }));
    assert_eq!(core_empty.mnemonic_passphrase().unwrap(), None);
    let plain = wallet(STRICT, b"", MnemonicCompat::Bip39);
    assert_eq!(plain.mnemonic_passphrase().unwrap(), None);
    for (passphrase, compat) in
        [(&b"TREZOR\0"[..], MnemonicCompat::DashCore), (&b"TREZOR"[..], MnemonicCompat::Bip39)]
    {
        let w = wallet(STRICT, passphrase, compat);
        assert_eq!(w.mnemonic_passphrase().unwrap().map(|p| p.expose()), Some(passphrase));
    }
}

/// BIP39 vector (Trezor `vectors.json`, entropy 00…00, passphrase "TREZOR").
const TREZOR_SEED_HEX: &str = "c55257c360c07c72029aebc1b53c05ed0362ada38ead3e3e9efa3708e53495531f09a6987599d18264c1e1c92f2cf141630c7a3c4ab7c81b2f001698e7463b04";

/// A BIP39 variant-5 wallet's seed is the BIP39 seed of phrase and passphrase (NFKD applied to
/// the passphrase), and its root key is that seed's master key.
#[test]
fn bip39_variant_5_seed_is_the_bip39_seed() {
    let w = wallet(STRICT, b"TREZOR", MnemonicCompat::Bip39);
    let seed = w.seed_zeroizing().unwrap().unwrap();
    assert_eq!(seed.to_lower_hex_string(), TREZOR_SEED_HEX);
    let reference = bip39::Mnemonic::parse(STRICT).unwrap();
    assert_eq!(*seed, reference.to_seed("TREZOR"));

    // NFKD changes this passphrase; the wallet keeps the bytes as given.
    let pass = "TREZOR-\u{c5}";
    let w = wallet(STRICT, pass.as_bytes(), MnemonicCompat::Bip39);
    let seed = w.seed_zeroizing().unwrap().unwrap();
    assert_eq!(*seed, reference.to_seed(pass));
    assert_eq!(*seed, reference.to_seed("TREZOR-A\u{30a}"));
    assert_eq!(w.mnemonic_passphrase().unwrap().unwrap().expose(), pass.as_bytes());
    let master = ExtendedPrivKey::new_master(Network::Testnet, &seed[..]).unwrap();
    let WalletType::MnemonicWithPassphrase {
        root_extended_private_key: root,
        ..
    } = &w.wallet_type
    else {
        panic!("variant");
    };
    assert_eq!(root.to_extended_priv_key(Network::Testnet), master);
}

/// The deprecated `wallet_seed_bytes` gives the passphrase seed for variant 5 (design §2.4.1).
#[test]
#[allow(deprecated)]
fn wallet_seed_bytes_uses_the_passphrase() {
    let w = wallet(STRICT, b"TREZOR", MnemonicCompat::Bip39);
    assert_eq!(w.wallet_seed_bytes().unwrap().to_lower_hex_string(), TREZOR_SEED_HEX);
    let core = wallet(CORE_ONLY, b"TREZOR\0", MnemonicCompat::DashCore);
    assert_eq!(core.wallet_seed_bytes().unwrap(), *core_seed(CORE_ONLY.as_bytes(), b"TREZOR\0"),);
    let plain = wallet(STRICT, b"", MnemonicCompat::Bip39);
    assert_ne!(plain.wallet_seed_bytes().unwrap(), w.wallet_seed_bytes().unwrap());
}

/// D108: a BIP39 variant-5 wallet whose passphrase was made non-UTF-8 in memory has corrupt
/// secrets: `seed_zeroizing` is `Err(Corrupt)` and `wallet_seed_bytes` is `None`.
#[test]
#[allow(deprecated)]
fn non_utf8_bip39_passphrase_in_memory_is_corrupt() {
    let mut w = wallet(STRICT, b"TREZOR", MnemonicCompat::Bip39);
    let WalletType::MnemonicWithPassphrase {
        passphrase,
        ..
    } = &mut w.wallet_type
    else {
        panic!("variant");
    };
    *passphrase = Passphrase::from_slice(&[0xFF, 0xFE]);
    assert_eq!(w.seed_zeroizing().unwrap_err(), CryptoError::Corrupt);
    assert_eq!(w.wallet_seed_bytes(), None);
}
