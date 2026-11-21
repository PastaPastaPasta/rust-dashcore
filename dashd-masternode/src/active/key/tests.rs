use std::collections::BTreeSet;

use dash_args::Program;
use dash_pkc::bls::BlsPkBytes;
use dash_pkc::bls::core::{BlsIesMultiBlobs, BlsVerdict, public_key, verify_insecure};
use serde_json::Value;

use super::*;
use crate::test_log::logged;

const FIXTURE: &str = include_str!("../../../tests/fixtures/am1-key/core-v24.json");
const SECRET: &str = "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20";

fn infinity<const N: usize>() -> [u8; N] {
    let mut bytes = [0; N];
    bytes[0] = 0xc0;
    bytes
}

fn fixture() -> Value {
    serde_json::from_str(FIXTURE).unwrap()
}

fn args_for(row: &Value) -> ArgsManager {
    let args = ArgsManager::new();
    Program::Dashd.setup_args(&args);
    match row["source"].as_str().unwrap() {
        "settings" => {
            let value =
                SettingsValue::read(row["value_json"].as_str().unwrap().as_bytes()).unwrap();
            args.lock_settings(|s| s.rw_settings.insert("masternodeblsprivkey".into(), value));
        }
        "command-line" => {
            let argv: Vec<&str> = std::iter::once("dashd")
                .chain(row["argv"].as_array().unwrap().iter().map(|arg| arg.as_str().unwrap()))
                .collect();
            args.parse_parameters(&argv).unwrap();
        }
        "config" => {
            args.read_config_stream(
                &format!("masternodeblsprivkey={}\n", row["value"].as_str().unwrap()),
                "dash.conf",
                false,
            )
            .unwrap();
        }
        _ => unreachable!(),
    }
    args
}

#[test]
fn parse_matches_the_recorded_core_v24_matrix() {
    let fixture = fixture();
    let mut group_order_cases = 0;
    for row in fixture["rows"].as_array().unwrap() {
        if row["name"] == "group-order" {
            group_order_cases += 1;
            assert_eq!(row["outcome"], "key");
            let args = args_for(row);
            assert_eq!(args.get_arg_opt("-masternodeblsprivkey").unwrap(), Some(hex(&GROUP_ORDER)));
        }
        let result = parse(&args_for(row));
        let context = format!("{}/{}", row["source"], row["name"]);
        match row["outcome"].as_str().unwrap() {
            "inactive" => assert!(result.unwrap().is_none(), "{context}"),
            "invalid" => {
                let error = result.unwrap_err();
                assert_eq!(error, ActiveMnInitError::InvalidKey, "{context}");
                assert_eq!(error.to_string(), row["diagnostic"], "{context}");
            }
            "exception" => {
                let error = result.unwrap_err();
                assert!(matches!(error, ActiveMnInitError::Argument(_)), "{context}");
                assert_eq!(error.to_string(), row["diagnostic"], "{context}");
            }
            "key" => {
                let key = result.unwrap().unwrap();
                assert_eq!(
                    hex(&key.pubkey().to_bytes(BlsSchemeKind::Legacy)),
                    row["legacy"],
                    "{context}"
                );
                assert_eq!(
                    hex(&key.pubkey().to_bytes(BlsSchemeKind::Basic)),
                    row["basic"],
                    "{context}"
                );
            }
            _ => unreachable!(),
        }
    }
    assert_eq!(group_order_cases, 2);
}

#[test]
fn the_group_order_signs_to_infinity_in_both_schemes() {
    let args = ArgsManager::new();
    args.force_set_arg("-masternodeblsprivkey", &hex(&GROUP_ORDER));
    let key = parse(&args).unwrap().unwrap();
    for legacy in [false, true] {
        let scheme = BlsSchemeKind::from_legacy(legacy);
        let signature = key.sign(&[42; 32], legacy);
        assert!(signature.is_valid());
        assert_eq!(signature.to_bytes(scheme), infinity::<96>());
    }
    assert_eq!(key.sign_basic(&[42; 32]), infinity::<96>());
}

#[test]
fn network_public_keys_and_constructor_logs_match_core_v24() {
    let fixture = fixture();
    for row in fixture["network_keys"].as_array().unwrap() {
        let (key, lines) = logged(|| parse(&args_for(row)).unwrap().unwrap());
        assert_eq!(hex(&key.pubkey().to_bytes(BlsSchemeKind::Legacy)), row["legacy"]);
        assert_eq!(hex(&key.pubkey().to_bytes(BlsSchemeKind::Basic)), row["basic"]);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].0, tracing::Level::INFO);
        assert_eq!(lines[0].1, row["log"]);
        assert!(row["network_public"] == row["legacy"] || row["network_public"] == row["basic"]);
    }
}

#[test]
fn signatures_verify_in_the_requested_scheme() {
    let args = ArgsManager::new();
    args.force_set_arg("-masternodeblsprivkey", SECRET);
    let key = parse(&args).unwrap().unwrap();
    for legacy in [false, true] {
        let hash = core::array::from_fn(|i| i as u8);
        let signature = key.sign(&hash, legacy);
        let scheme = BlsSchemeKind::from_legacy(legacy);
        assert_eq!(verify_insecure(&signature, &key.pubkey(), &hash, scheme), BlsVerdict::Valid);
        assert_ne!(
            verify_insecure(&signature, &key.pubkey(), &[42; 32], scheme),
            BlsVerdict::Valid
        );
    }
    assert_eq!(
        key.sign_basic(&[42; 32]),
        key.sign(&[42; 32], false).to_bytes(BlsSchemeKind::Basic)
    );
}

#[test]
fn signatures_match_recorded_core_v24_bytes() {
    let fixture = fixture();
    assert_eq!(fixture["signatures"]["core_commit"], fixture["core_commit"]);
    let rows = fixture["signatures"]["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 4);
    let mut network_indices = BTreeSet::new();
    for row in rows {
        let network_index = row["network_index"].as_u64().unwrap();
        network_indices.insert(network_index);
        let network_key = fixture["network_keys"]
            .as_array()
            .unwrap()
            .iter()
            .find(|key| key["index"] == network_index)
            .unwrap();
        let key = parse(&args_for(network_key)).unwrap().unwrap();
        let hash: [u8; 32] =
            parse_hex(row["hash_bytes"].as_str().unwrap()).unwrap().as_slice().try_into().unwrap();
        for (legacy, field) in [(true, "legacy"), (false, "basic")] {
            assert_eq!(
                hex(&key.sign(&hash, legacy).to_bytes(BlsSchemeKind::from_legacy(legacy))),
                row[field],
                "network key {network_index}, hash {}, {field}",
                row["hash_bytes"]
            );
        }
        assert_eq!(hex(&key.sign_basic(&hash)), row["sign_basic"]);
    }
    assert_eq!(network_indices.len(), 2);
}

#[test]
fn load_sign_and_debug_never_expose_the_operator_secret() {
    let fixture = fixture();
    for row in fixture["network_keys"].as_array().unwrap() {
        let args = args_for(row);
        args.log_args();
        let arg_logs = args.take_buffered_log().concat();
        assert!(arg_logs.contains("masternodeblsprivkey=****"));
        let (_, lines) = logged(|| {
            let key = parse(&args).unwrap().unwrap();
            assert_eq!(format!("{key:?}"), "OperatorKey(<redacted>)");
            tracing::info!("{key:?}");
            tracing::info!("{:?}", key.pubkey());
            for i in 0u32..1_000 {
                let mut hash = [0; 32];
                hash[..4].copy_from_slice(&i.to_le_bytes());
                let signature = key.sign(&hash, i % 2 == 0);
                tracing::info!("{signature:?}");
                tracing::info!("{:?}", key.sign_basic(&hash));
            }
            for error in [
                ActiveMnInitError::InvalidKey,
                ActiveMnInitError::Argument(RuntimeError(
                    "JSON value of type array is not of expected type string".into(),
                )),
                ActiveMnInitError::Argument(RuntimeError(
                    "JSON value of type object is not of expected type string".into(),
                )),
            ] {
                tracing::info!("{error:?}: {error}");
            }
        });
        let mut output = arg_logs;
        for (_, line) in lines {
            output.push_str(&line);
        }
        let secret = row["secret"].as_str().unwrap();
        let config_args = ArgsManager::new();
        Program::Dashd.setup_args(&config_args);
        config_args
            .read_config_stream(&format!("masternodeblsprivkey={secret}\n"), "dash.conf", false)
            .unwrap();
        config_args.log_args();
        let config_logs = config_args.take_buffered_log().concat();
        assert!(config_logs.contains("masternodeblsprivkey=****"));
        output.push_str(&config_logs);
        let raw = parse_hex(secret).unwrap();
        for forbidden in [secret.to_owned(), secret.to_uppercase(), hex(&raw)] {
            assert!(!output.contains(&forbidden), "secret material reached the log");
        }
    }
}

#[test]
fn invalid_settings_and_errors_never_echo_secret_values() {
    let args = ArgsManager::new();
    for value in [
        SettingsValue::Arr(vec![SettingsValue::Str(SECRET.into())]),
        SettingsValue::Obj(vec![(SECRET.into(), SettingsValue::Str(SECRET.into()))]),
        SettingsValue::Str(format!("{SECRET}zz")),
    ] {
        args.lock_settings(|s| s.rw_settings.insert("masternodeblsprivkey".into(), value));
        let (error, lines) = logged(|| parse(&args).unwrap_err());
        let output = format!("{error:?} {error} {lines:?}");
        assert!(!output.contains(SECRET));
    }
}

#[test]
fn dropping_the_operator_key_cleanses_its_locked_chunk() {
    let args = ArgsManager::new();
    args.force_set_arg("-masternodeblsprivkey", SECRET);
    let key = parse(&args).unwrap().unwrap();
    assert_eq!(key.sk.as_slice(), parse_hex(SECRET).unwrap().as_slice());
    dashd_lockedpool::last_drop();
    drop(key);
    assert_eq!(dashd_lockedpool::last_drop(), Some(vec![0; 32]));
}

#[test]
fn every_owned_setting_string_is_zeroized_including_nested_errors() {
    let mut setting = SecretSetting(SettingsValue::Obj(vec![(
        SECRET.into(),
        SettingsValue::Arr(vec![
            SettingsValue::Str(SECRET.into()),
            SettingsValue::Num("1234".into()),
            SettingsValue::Null,
            SettingsValue::Bool(true),
        ]),
    )]));
    setting.zeroize();
    let SettingsValue::Obj(entries) = &setting.0 else {
        unreachable!()
    };
    assert!(entries[0].0.is_empty());
    let SettingsValue::Arr(values) = &entries[0].1 else {
        unreachable!()
    };
    assert_eq!(values[0], SettingsValue::Str(String::new()));
    assert_eq!(values[1], SettingsValue::Num(String::new()));
}

fn operator() -> OperatorKey {
    let args = ArgsManager::new();
    args.force_set_arg("-masternodeblsprivkey", SECRET);
    parse(&args).unwrap().unwrap()
}

fn scalar(n: u8) -> BlsSkValue {
    let mut bytes = [0; 32];
    bytes[31] = n;
    BlsSkValue::from_canonical(&bytes, BlsSchemeKind::Basic).unwrap()
}

/// The contributions to three members, the operator second, as `dkgsession.cpp:83-95` encrypts
/// them; each plaintext is `scalar(10 + i)` under `scheme`.
fn contributions(scheme: BlsSchemeKind) -> BlsIesMultiBlobs {
    let operator = operator().pubkey();
    let others = [public_key(&scalar(1)), public_key(&scalar(2))];
    let plaintexts: Vec<_> = (0..3).map(|i| scalar(10 + i).to_bytes(scheme)).collect();
    let recipients = [
        (&others[0], &plaintexts[0][..]),
        (&operator, &plaintexts[1][..]),
        (&others[1], &plaintexts[2][..]),
    ];
    let (blobs, ok) = BlsIesMultiBlobs::encrypt_with(&scalar(7), &[0x5a; 32], &recipients);
    assert!(ok);
    blobs
}

fn blob_bytes(blobs: &BlsIesMultiBlobs, idx: usize, scheme: BlsSchemeKind) -> BlsIesBlobBytes {
    let blob = blobs.get(idx).unwrap();
    BlsIesBlobBytes::new(
        BlsPkBytes::from_bytes(blob.ephemeral.to_bytes(scheme)),
        blob.iv_seed,
        blob.data,
    )
}

/// The operator's own blob decrypts to its share under either scheme; another index's IV does
/// not give it, nor does another recipient's blob.
// Core: v24.0.0 src/active/masternode.cpp:278-284; src/bls/bls_ies.h:47-60
#[test]
fn decrypt_sk_round_trips_in_both_schemes() {
    let key = operator();
    for scheme in [BlsSchemeKind::Legacy, BlsSchemeKind::Basic] {
        let blobs = contributions(scheme);
        let mine = blob_bytes(&blobs, 1, scheme);
        assert_eq!(key.decrypt_sk(&mine, 1, scheme), Some(scalar(11)), "{scheme:?}");
        for idx in [0, 2] {
            assert_ne!(key.decrypt_sk(&mine, idx, scheme), Some(scalar(11)), "{scheme:?}");
        }
        assert_ne!(key.decrypt_sk(&blob_bytes(&blobs, 0, scheme), 0, scheme), Some(scalar(10)));
    }
}

/// An ephemeral key that does not read as a public key under the scheme is Core's false.
// Core: v24.0.0 src/bls/bls_ies.cpp:40-53; src/bls/bls.h:310-320
#[test]
fn decrypt_sk_refuses_an_invalid_ephemeral_key() {
    let key = operator();
    for scheme in [BlsSchemeKind::Legacy, BlsSchemeKind::Basic] {
        let blob = blob_bytes(&contributions(scheme), 1, scheme);
        for ephemeral in [[0; 48], [0xff; 48]] {
            let bad = BlsIesBlobBytes::new(
                BlsPkBytes::from_bytes(ephemeral),
                *blob.iv_seed(),
                blob.data().to_vec(),
            );
            assert_eq!(key.decrypt_sk(&bad, 1, scheme), None, "{scheme:?}");
        }
    }
}

/// The active masternode decrypts through `OperatorDecrypt` and names its registration once it
/// has one.
#[test]
fn the_active_masternode_is_the_intake_operator() {
    use dash_llmq::qdata::source::OperatorDecrypt;

    let active = crate::active::ActiveMasternode::new(operator());
    assert_eq!(OperatorDecrypt::pro_tx_hash(&active), None);
    active.st.write().pro_tx_hash = Some([3; 32]);
    assert_eq!(
        OperatorDecrypt::pro_tx_hash(&active),
        Some(dash_evo::types::ProTxHash::from([3; 32]))
    );
    let blob = blob_bytes(&contributions(BlsSchemeKind::Basic), 1, BlsSchemeKind::Basic);
    assert_eq!(
        OperatorDecrypt::decrypt_sk(&active, &blob, 1, BlsSchemeKind::Basic),
        Some(scalar(11))
    );
}

fn secret() -> BlsSkValue {
    let bytes = parse_hex(SECRET).unwrap();
    BlsSkValue::from_canonical(bytes.as_slice().try_into().unwrap(), BlsSchemeKind::Basic).unwrap()
}

/// The operator key DK1's sessions use: its public key, its signature in either scheme and its
/// decryption of a session's contributions equal dash-llmq's over the same secret, and the
/// contribution decryption agrees with `qdata` intake's single-blob one.
// Core: v24.0.0 src/active/masternode.cpp:278-312; src/bls/bls_ies.h:98-112
#[test]
fn the_dkg_operator_key_matches_dash_llmqs_bls_calls() {
    let key = operator();
    let secret = secret();
    assert_eq!(key.pubkey(), public_key(&secret));
    for legacy in [false, true] {
        let hash = [7; 32];
        let want = dash_llmq::dkg::bls::sign(&secret, &hash, legacy);
        assert_eq!(key.sign(&hash, legacy), want, "{legacy}");
    }
    for scheme in [BlsSchemeKind::Legacy, BlsSchemeKind::Basic] {
        let blobs = contributions(scheme);
        let mine = key.decrypt_share(&blobs, 1, scheme);
        assert_eq!(mine, Some(scalar(11)), "{scheme:?}");
        assert_eq!(mine, decrypt_share(&blobs, 1, &secret, scheme), "{scheme:?}");
        assert_eq!(mine, key.decrypt_sk(&blob_bytes(&blobs, 1, scheme), 1, scheme), "{scheme:?}");
        for idx in [0, 2] {
            assert_ne!(key.decrypt_share(&blobs, idx, scheme), Some(scalar(10 + idx as u8)));
        }
    }
}

/// The single-member sig-share signer signs the hash as given, in the scheme asked for, with
/// the operator key's `Sign`. The group order's infinity signature is valid in Core, so it is
/// returned, not refused.
// Core: v24.0.0 src/llmq/signing_shares.cpp:1580-1588
#[test]
fn the_sig_share_signer_is_the_operator_keys_sign() {
    let key = operator();
    let hash = [9; 32];
    for scheme in [BlsSchemeKind::Legacy, BlsSchemeKind::Basic] {
        let want = key.sign(&hash, scheme == BlsSchemeKind::Legacy);
        assert_eq!(OperatorSigner::sign(&key, &hash, scheme), Some(want), "{scheme:?}");
    }
    let args = ArgsManager::new();
    args.force_set_arg("-masternodeblsprivkey", &hex(&GROUP_ORDER));
    let order = parse(&args).unwrap().unwrap();
    for scheme in [BlsSchemeKind::Legacy, BlsSchemeKind::Basic] {
        let sig = OperatorSigner::sign(&order, &hash, scheme).unwrap();
        assert_eq!(sig.to_bytes(scheme), infinity::<96>(), "{scheme:?}");
    }
}

/// The active masternode signs and decrypts for DK1 with its own key, is the sig-share signer
/// shared as `Arc<dyn OperatorSigner + Send + Sync>`, and none of its signing or decryption logs.
#[test]
fn the_active_masternode_signs_and_decrypts_with_its_key_and_is_the_sig_share_signer() {
    use std::sync::Arc;

    let active = Arc::new(crate::active::ActiveMasternode::new(operator()));
    let key = operator();
    let hash = [5; 32];
    let blobs = contributions(BlsSchemeKind::Basic);
    assert_eq!(active.operator_key().pubkey(), key.pubkey());
    let signer: Arc<dyn OperatorSigner + Send + Sync> = active.clone();
    let ((), lines) = logged(|| {
        for legacy in [false, true] {
            assert_eq!(active.sign(&hash, legacy), key.sign(&hash, legacy));
            let scheme = BlsSchemeKind::from_legacy(legacy);
            assert_eq!(signer.sign(&hash, scheme), Some(key.sign(&hash, legacy)));
        }
        assert_eq!(active.decrypt_share(&blobs, 1, BlsSchemeKind::Basic), Some(scalar(11)));
    });
    assert!(lines.is_empty(), "{lines:?}");
}
