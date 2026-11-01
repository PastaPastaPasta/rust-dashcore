//! Dash Core's mnemonic rules against vectors Core produced: its `bip39_vectors.json`, the
//! dashwallet-desktop quirk corpus (Core's `CMnemonic` compiled from source, plus regtest
//! `upgradetohd`; provenance in `tests/data/PROVENANCE.md`) and the `wallet_upgradetohd.py`
//! literals (`transcribed/bip39_functional.json` in dash-core-vectors).

use dash_core_vectors::cases::{load_bip39, BIP39_PASSPHRASE};
use dash_core_vectors::read_core_text;
use dashcore_hashes::{sha256, Hash};
use key_wallet::account::ECDSAAddressDerivation;
use key_wallet::mnemonic::{
    core_check, core_seed, DashCoreMnemonic, MnemonicCompat, WalletMnemonic,
    CORE_LEGACY_MAX_PASSPHRASE_BYTES,
};
use key_wallet::wallet::initialization::WalletAccountCreationOptions;
use key_wallet::wallet::WalletType;
use key_wallet::{
    DerivationPath, ExtendedPrivKey, Language, Mnemonic, Network, Passphrase, Wallet,
};
use serde_json::Value;

const QUIRKS_SHA256: &str = "85685865c0cd61ed44c6b3447f0c02a0d069c73d205d74109f8b7980cc5b2953";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

fn quirks() -> Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/core_bip39_quirks.json");
    let bytes = std::fs::read(path).unwrap();
    assert_eq!(sha256::Hash::hash(&bytes).to_string(), QUIRKS_SHA256, "core_bip39_quirks.json");
    serde_json::from_slice(&bytes).unwrap()
}

fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or_else(|| panic!("{key} in {v}"))
}

/// The wallet's master key in base58 (`xprv` on mainnet, `tprv` elsewhere).
fn root_xprv(wallet: &Wallet) -> String {
    wallet.derive_extended_private_key(&DerivationPath::master()).unwrap().to_string()
}

fn master(seed: &[u8], network: Network) -> String {
    ExtendedPrivKey::new_master(network, seed).unwrap().to_string()
}

/// `wallet/test/bip39_tests.cpp:22-62`: `FromData(entropy)` is the phrase, `Check` accepts it,
/// `ToSeed(phrase, "TREZOR")` is the seed and the master key is the xprv. Both modes agree on
/// these (strict phrases, ASCII passphrase).
#[test]
fn core_bip39_vectors() {
    let cases = load_bip39(&read_core_text("src/test/data/bip39_vectors.json")).unwrap();
    assert_eq!(cases.len(), 24);
    for case in cases {
        let core = DashCoreMnemonic::from_entropy(&case.entropy).unwrap();
        assert_eq!(core.phrase().expose(), case.mnemonic.as_bytes(), "case {}", case.index);
        assert!(core_check(case.mnemonic.as_bytes()));
        assert!(core.is_strict_bip39());
        assert_eq!(DashCoreMnemonic::parse(case.mnemonic.as_bytes()).unwrap(), core);

        let seed = core.to_seed(BIP39_PASSPHRASE.as_bytes());
        assert_eq!(hex(&seed[..]), case.seed_hex, "case {}", case.index);
        assert_eq!(master(&seed[..], Network::Mainnet), case.xprv, "case {}", case.index);

        let bip39 = Mnemonic::from_entropy(&case.entropy, Language::English).unwrap();
        assert_eq!(*bip39.to_seed_zeroizing(BIP39_PASSPHRASE), *seed);

        for compat in [MnemonicCompat::Bip39, MnemonicCompat::DashCore] {
            let wallet = Wallet::from_phrase(
                &case.mnemonic,
                Passphrase::from_slice(BIP39_PASSPHRASE.as_bytes()),
                compat,
                Network::Mainnet,
                WalletAccountCreationOptions::None,
            )
            .unwrap();
            assert_eq!(root_xprv(&wallet), case.xprv);
        }
    }
}

/// The 513 `Check` cases: `core_check` is Core's verdict, `strict_check` strict BIP39's, and
/// each word count's entropy encodes (`FromData`) to the one strict phrase of its group.
#[test]
fn quirk_corpus_check_cases() {
    let corpus = quirks();
    let cases = corpus["check"].as_array().unwrap();
    assert_eq!(cases.len(), 513);
    let (mut core_only, mut both) = (0, 0);
    for case in cases {
        let phrase = str_of(case, "mnemonic");
        let core = case["core_check"].as_bool().unwrap();
        let strict = case["strict_check"].as_bool().unwrap();
        let name = str_of(case, "case");

        assert_eq!(core_check(phrase.as_bytes()), core, "{name}");
        assert_eq!(DashCoreMnemonic::parse(phrase.as_bytes()).is_ok(), core, "{name}");
        assert_eq!(WalletMnemonic::parse(phrase, MnemonicCompat::DashCore).is_ok(), core, "{name}");
        let portability = WalletMnemonic::portability(phrase.as_bytes(), b"");
        assert_eq!((portability.core_accepts, portability.bip39_accepts), (core, strict), "{name}");
        if strict {
            assert!(WalletMnemonic::parse(phrase, MnemonicCompat::Bip39).is_ok(), "{name}");
        }
        if let Some(entropy) = case["entropy"].as_str() {
            let encoded = DashCoreMnemonic::from_entropy(&unhex(entropy)).unwrap();
            assert_eq!(encoded.phrase().expose() == phrase.as_bytes(), strict, "{name}");
        }
        match (core, strict) {
            (true, false) => core_only += 1,
            (true, true) => both += 1,
            (false, true) => panic!("{name}: strict BIP39 accepts what Core refuses"),
            (false, false) => {}
        }
    }
    assert_eq!((core_only, both), (131, 7));
}

/// The 12 seed cases: `core_seed` is Core's `ToSeed` (raw bytes, salt cut at 256 bytes);
/// `bip39_seed` the BIP39 seed, which `Mnemonic` reproduces for strict phrases. Where Core
/// accepts the phrase, `seeds_agree` predicts whether the two are equal.
#[test]
fn quirk_corpus_seed_cases() {
    let corpus = quirks();
    let cases = corpus["seed"].as_array().unwrap();
    assert_eq!(cases.len(), 12);
    for case in cases {
        let name = str_of(case, "case");
        let phrase = str_of(case, "mnemonic");
        let passphrase = str_of(case, "passphrase");
        let core = str_of(case, "core_seed");
        let bip39 = str_of(case, "bip39_seed");

        assert_eq!(hex(&core_seed(phrase.as_bytes(), passphrase.as_bytes())[..]), core, "{name}");
        let portability = WalletMnemonic::portability(phrase.as_bytes(), passphrase.as_bytes());
        if !portability.core_accepts {
            // "mnemonic is hashed as given (double space)": ToSeed runs no Check.
            assert!(name.contains("double space"), "{name}");
            continue;
        }
        let parsed = DashCoreMnemonic::parse(phrase.as_bytes()).unwrap();
        assert_eq!(hex(&parsed.to_seed(passphrase.as_bytes())[..]), core, "{name}");
        let wallet_mnemonic = WalletMnemonic::parse(phrase, MnemonicCompat::DashCore).unwrap();
        let seed = wallet_mnemonic.to_seed(&Passphrase::from_slice(passphrase.as_bytes())).unwrap();
        assert_eq!(hex(&seed[..]), core, "{name}");

        assert!(portability.bip39_accepts, "{name}");
        let strict = Mnemonic::from_phrase(phrase).unwrap();
        assert_eq!(hex(&strict.to_seed_zeroizing(passphrase)[..]), bip39, "{name}");
        assert_eq!(portability.seeds_agree(), core == bip39, "{name}");
    }
}

/// The 24 Trezor vectors as the corpus recorded them from Core's `CMnemonic`.
#[test]
fn quirk_corpus_trezor_cases() {
    let corpus = quirks();
    let cases = corpus["trezor"].as_array().unwrap();
    assert_eq!(cases.len(), 24);
    for case in cases {
        let phrase = str_of(case, "mnemonic");
        let core = DashCoreMnemonic::from_entropy(&unhex(str_of(case, "entropy"))).unwrap();
        assert_eq!(core.phrase().expose(), phrase.as_bytes());
        let seed = core.to_seed(str_of(case, "passphrase").as_bytes());
        assert_eq!(hex(&seed[..]), str_of(case, "seed"));
        assert_eq!(master(&seed[..], Network::Mainnet), str_of(case, "xprv"));
    }
}

/// Regtest dashd `upgradetohd`: the legacy wallet's seed is Core's `ToSeed` (or Core's length
/// error above 256 bytes, the limit our constant states) and the descriptor wallet's master key
/// is the master key of that seed, on every passphrase shape and on weak-checksum phrases.
#[test]
fn quirk_corpus_dashd_cases() {
    let corpus = quirks();
    let cases = corpus["dashd"]["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 12);
    let mut legacy_errors = 0;
    for case in cases {
        let name = str_of(case, "case");
        let phrase = str_of(case, "mnemonic");
        let passphrase = str_of(case, "passphrase");
        let parsed = DashCoreMnemonic::parse(phrase.as_bytes()).unwrap();
        let seed = parsed.to_seed(passphrase.as_bytes());

        match (case.get("legacy_seed"), case.get("legacy_error")) {
            (Some(legacy), None) => {
                assert!(passphrase.len() <= CORE_LEGACY_MAX_PASSPHRASE_BYTES, "{name}");
                assert_eq!(hex(&seed[..]), legacy.as_str().unwrap(), "{name}");
            }
            (None, Some(error)) => {
                assert!(passphrase.len() > CORE_LEGACY_MAX_PASSPHRASE_BYTES, "{name}");
                let error = error.as_str().unwrap();
                assert!(
                    error.contains(&format!(
                        "at most {CORE_LEGACY_MAX_PASSPHRASE_BYTES} characters"
                    )),
                    "{name}: {error}"
                );
                legacy_errors += 1;
            }
            _ => panic!("{name}: expected exactly one of legacy_seed and legacy_error"),
        }
        let xprv = str_of(case, "descriptor_master_xprv");
        assert_eq!(master(&seed[..], Network::Regtest), xprv, "{name}");

        let wallet = Wallet::from_phrase(
            phrase,
            Passphrase::from_slice(passphrase.as_bytes()),
            MnemonicCompat::DashCore,
            Network::Regtest,
            WalletAccountCreationOptions::None,
        )
        .unwrap();
        assert_eq!(root_xprv(&wallet), xprv, "{name}");
        assert_eq!(*wallet.seed_zeroizing().unwrap().unwrap(), *seed, "{name}");
    }
    assert_eq!(legacy_errors, 1);
}

/// `wallet_upgradetohd.py:234-238, 258-267`: the phrase without and with `custom-passphrase`
/// owns the address the test paid (BIP44 external chain, within Core's 1,000-key keypool), and
/// `custom-passphrase\0` is a different seed that owns neither address.
#[test]
fn upgradetohd_functional_vectors() {
    let doc: Value =
        serde_json::from_str(&read_core_text("transcribed/bip39_functional.json")).unwrap();
    assert_eq!(doc["source"], "test/functional/wallet_upgradetohd.py");
    let records = doc["records"].as_array().unwrap();
    assert_eq!(records.len(), 3);

    let receive_addresses = |phrase: &str, passphrase: &str| -> Vec<String> {
        let wallet = Wallet::from_phrase(
            phrase,
            Passphrase::from_slice(passphrase.as_bytes()),
            MnemonicCompat::DashCore,
            Network::Regtest,
            WalletAccountCreationOptions::BIP44AccountsOnly([0].into()),
        )
        .unwrap();
        let account = wallet.accounts.standard_bip44_accounts.get(&0).unwrap();
        let mut addresses = account.derive_receive_addresses(0, 1000).unwrap();
        addresses.extend(account.derive_change_addresses(0, 1000).unwrap());
        addresses.into_iter().map(|a| a.to_string()).collect()
    };

    let mut seeds = Vec::new();
    for record in records {
        let phrase = str_of(record, "mnemonic");
        let passphrase = str_of(record, "passphrase");
        assert!(core_check(phrase.as_bytes()));
        let addresses = receive_addresses(phrase, passphrase);
        if let Some(owned) = record["owns"].as_str() {
            let index = addresses.iter().position(|a| a == owned);
            // The first receive address: Core's keypool hands it out first.
            assert_eq!(index, Some(0), "{passphrase:?} owns {owned}");
        } else {
            assert!(passphrase.ends_with('\0'));
            for other in record["owns_none_of"].as_array().unwrap() {
                assert!(!addresses.iter().any(|a| a == other.as_str().unwrap()));
            }
            assert_eq!(record["balance"], 0);
        }
        seeds.push(*core_seed(phrase.as_bytes(), passphrase.as_bytes()));
    }
    assert_ne!(seeds[1], seeds[2], "the NUL byte is part of the passphrase");
}

/// Empty-passphrase BIP39 keeps today's wallet: variant 0, the same bytes and the same ID as
/// `from_mnemonic`; a non-empty passphrase or DashCore mode is variant 5 (D2).
#[test]
fn empty_bip39_passphrase_is_the_plain_wallet() {
    let phrase =
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
    for network in [Network::Mainnet, Network::Testnet] {
        let plain = Wallet::from_mnemonic(
            Mnemonic::from_phrase(phrase).unwrap(),
            network,
            WalletAccountCreationOptions::Default,
        )
        .unwrap();
        let via_phrase = Wallet::from_phrase(
            phrase,
            Passphrase::new(Vec::new()),
            MnemonicCompat::Bip39,
            network,
            WalletAccountCreationOptions::Default,
        )
        .unwrap();
        assert!(matches!(via_phrase.wallet_type, WalletType::Mnemonic { .. }));
        assert_eq!(via_phrase.wallet_id, plain.wallet_id);
        assert_eq!(via_phrase.to_bytes().unwrap(), plain.to_bytes().unwrap());

        for (passphrase, compat) in
            [(&b"x"[..], MnemonicCompat::Bip39), (&b""[..], MnemonicCompat::DashCore)]
        {
            let wallet = Wallet::from_phrase(
                phrase,
                Passphrase::from_slice(passphrase),
                compat,
                network,
                WalletAccountCreationOptions::None,
            )
            .unwrap();
            assert!(matches!(wallet.wallet_type, WalletType::MnemonicWithPassphrase { .. }));
        }
    }
}
