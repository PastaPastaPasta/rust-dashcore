//! `{:?}` and `{:#?}` of every secret-bearing type reachable from `Wallet` show no secret
//! (design §2.3, D40): no phrase word, no passphrase, no seed, private key or chain code byte in
//! hex, decimal-array or raw form, and no WIF or xprv string. The chain code counts as secret.

use std::collections::BTreeSet;

use dashcore_hashes::{sha256, Hash};
use key_wallet::mnemonic::{DashCoreMnemonic, WalletMnemonic};
use key_wallet::wallet::initialization::WalletAccountCreationOptions;
use key_wallet::wallet::root_extended_keys::RootExtendedPrivKey;
use key_wallet::{
    DerivationPath, ExtendedPrivKey, Language, Mnemonic, Network, Passphrase, SecretBytes, Seed,
    Wallet,
};

const PASSPHRASE: &str = "Zq8-unusual-passphrase";

/// One phrase and everything secret derived from it, plus the `Debug` output of every type.
struct Fixture {
    words: Vec<String>,
    strings: Vec<String>,
    bytes: Vec<Vec<u8>>,
    outputs: Vec<(String, String)>,
}

fn record(outputs: &mut Vec<(String, String)>, label: &str, value: &dyn std::fmt::Debug) {
    outputs.push((format!("{label} {{:?}}"), format!("{value:?}")));
    outputs.push((format!("{label} {{:#?}}"), format!("{value:#?}")));
}

fn key_secrets(key: &ExtendedPrivKey, strings: &mut Vec<String>, bytes: &mut Vec<Vec<u8>>) {
    bytes.push(key.private_key.to_secret_bytes().to_vec());
    bytes.push(key.chain_code.to_bytes().to_vec());
    for network in [Network::Mainnet, Network::Testnet] {
        let mut on_network = key.clone();
        on_network.network = network;
        strings.push(on_network.to_string());
        for compressed in [true, false] {
            let wif = dashcore::PrivateKey {
                compressed,
                network,
                inner: key.private_key,
            }
            .to_wif();
            strings.push(wif);
        }
    }
}

fn fixture(label: &str) -> Fixture {
    let entropy = sha256::Hash::hash(label.as_bytes()).to_byte_array();
    let mnemonic = Mnemonic::from_entropy(&entropy, Language::English).unwrap();
    let phrase = mnemonic.phrase_zeroizing().to_string();
    let core = DashCoreMnemonic::from_entropy(&entropy).unwrap();
    let seed_plain = mnemonic.to_seed_zeroizing("");
    let seed_pass = mnemonic.to_seed_zeroizing(PASSPHRASE);

    let mut strings = vec![phrase.clone(), PASSPHRASE.to_owned()];
    let mut bytes = vec![seed_plain.to_vec(), seed_pass.to_vec(), entropy.to_vec()];
    let mut outputs = Vec::new();

    for seed in [&seed_plain[..], &seed_pass[..]] {
        let master = ExtendedPrivKey::new_master(Network::Mainnet, seed).unwrap();
        key_secrets(&master, &mut strings, &mut bytes);
        let path: DerivationPath = "m/44'/5'/0'".parse().unwrap();
        let child = master.derive_priv(&path).unwrap();
        key_secrets(&child, &mut strings, &mut bytes);
        record(&mut outputs, "ExtendedPrivKey (master)", &master);
        record(&mut outputs, "ExtendedPrivKey (child)", &child);
        record(
            &mut outputs,
            "RootExtendedPrivKey",
            &RootExtendedPrivKey::new_master(seed).unwrap(),
        );
        record(&mut outputs, "Seed", &Seed::from_slice(seed).unwrap());
    }

    record(&mut outputs, "Mnemonic", &mnemonic);
    record(&mut outputs, "DashCoreMnemonic", &core);
    record(&mut outputs, "WalletMnemonic::Bip39", &WalletMnemonic::Bip39(mnemonic.clone()));
    record(&mut outputs, "WalletMnemonic::DashCore", &WalletMnemonic::DashCore(core.clone()));
    record(&mut outputs, "SecretBytes (phrase)", &SecretBytes::from_slice(phrase.as_bytes()));
    record(&mut outputs, "Passphrase", &Passphrase::from_slice(PASSPHRASE.as_bytes()));

    let options = WalletAccountCreationOptions::Default;
    let wallets = [
        ("variant 0", Wallet::from_mnemonic(mnemonic.clone(), Network::Testnet, options.clone())),
        (
            "variant 1",
            Wallet::from_seed(
                Seed::from_slice(&seed_plain[..]).unwrap(),
                Network::Testnet,
                options.clone(),
            ),
        ),
        (
            "variant 2",
            Wallet::from_extended_key(
                ExtendedPrivKey::new_master(Network::Testnet, &seed_plain[..]).unwrap(),
                options.clone(),
            ),
        ),
        (
            "variant 5 (Bip39)",
            Wallet::from_mnemonic_with_passphrase(
                WalletMnemonic::Bip39(mnemonic.clone()),
                Passphrase::from_slice(PASSPHRASE.as_bytes()),
                Network::Testnet,
                options.clone(),
            ),
        ),
        (
            "variant 5 (DashCore)",
            Wallet::from_mnemonic_with_passphrase(
                WalletMnemonic::DashCore(core.clone()),
                Passphrase::from_slice(PASSPHRASE.as_bytes()),
                Network::Testnet,
                options.clone(),
            ),
        ),
    ];
    for (name, wallet) in wallets {
        let wallet = wallet.unwrap();
        record(&mut outputs, &format!("WalletType {name}"), &wallet.wallet_type);
        record(&mut outputs, &format!("Wallet {name}"), &wallet);
    }
    let variants: BTreeSet<_> = outputs
        .iter()
        .filter(|(label, _)| label.starts_with("WalletType"))
        .map(|(_, out)| out.split([' ', '(', '{']).next().unwrap().to_owned())
        .collect();
    assert_eq!(
        variants,
        ["ExtendedPrivKey", "Mnemonic", "MnemonicWithPassphrase", "Seed"]
            .map(String::from)
            .into_iter()
            .collect()
    );

    Fixture {
        words: phrase.split(' ').map(String::from).collect(),
        strings,
        bytes,
        outputs,
    }
}

/// Alphabetic tokens of a `Debug` output.
fn tokens(output: &str) -> BTreeSet<&str> {
    output.split(|c: char| !c.is_ascii_alphabetic()).filter(|t| !t.is_empty()).collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Fails if `fixture`'s outputs show any of its secrets. A phrase word only counts when the
/// other fixture's output for the same type lacks it: field and variant names (some of them are
/// BIP39 words, like `network`) appear in both.
fn assert_redacted(fixture: &Fixture, other: &Fixture) {
    const WINDOW: usize = 8;
    for ((label, output), (other_label, other_output)) in fixture.outputs.iter().zip(&other.outputs)
    {
        assert_eq!(label, other_label);
        let structural = tokens(other_output);
        for token in tokens(output) {
            assert!(
                structural.contains(token) || !fixture.words.iter().any(|w| w == token),
                "{label} shows the phrase word {token:?}"
            );
        }
        for s in &fixture.strings {
            assert!(!output.contains(s.as_str()), "{label} shows a secret string");
        }
        for prefix in ["xprv", "tprv"] {
            assert!(!output.contains(prefix), "{label} shows an extended private key");
        }
        let compact: String = output.chars().filter(|c| !c.is_whitespace()).collect();
        let lower = output.to_ascii_lowercase();
        for secret in &fixture.bytes {
            for window in secret.windows(WINDOW) {
                assert!(!lower.contains(&hex(window)), "{label} shows secret bytes in hex");
                let decimal = window.iter().map(u8::to_string).collect::<Vec<_>>().join(",");
                assert!(!compact.contains(&decimal), "{label} shows secret bytes as numbers");
                assert!(
                    !output.as_bytes().windows(WINDOW).any(|w| w == window),
                    "{label} shows raw secret bytes"
                );
            }
        }
    }
}

#[test]
fn debug_output_shows_no_secret() {
    let a = fixture("wl1-2 debug scan A");
    let b = fixture("wl1-2 debug scan B");
    assert!(a.words.iter().all(|w| !b.words.contains(w)), "fixtures must not share a word");
    assert_redacted(&a, &b);
    assert_redacted(&b, &a);
}

/// The scan catches what it looks for: an output that prints a word, a key or a chain code
/// in any of the three forms fails.
#[test]
fn scan_detects_leaks() {
    let a = fixture("wl1-2 debug scan A");
    let leaks = [
        format!("Leak {{ word: {} }}", a.words[3]),
        format!("Leak({})", hex(&a.bytes[3])),
        format!("Leak({:?})", &a.bytes[4][..]),
        format!("Leak({:#?})", &a.bytes[4][..]),
        a.strings[2].clone(),
    ];
    for leak in leaks {
        let leaky = Fixture {
            words: a.words.clone(),
            strings: a.strings.clone(),
            bytes: a.bytes.clone(),
            outputs: vec![("leak".into(), leak.clone())],
        };
        let other = Fixture {
            words: vec![],
            strings: vec![],
            bytes: vec![],
            outputs: vec![("leak".into(), "Leak".into())],
        };
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assert_redacted(&leaky, &other)
        }))
        .is_err();
        assert!(caught, "not caught: {leak}");
    }
}
