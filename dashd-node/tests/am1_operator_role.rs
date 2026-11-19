//! AM1's key parser and the step-6 role agree for Core's recorded argument matrix.

use dash_args::{ArgsManager, Program, SettingsValue};
use dashd_masternode::active::key::parse;
use dashd_node::p2p::MasternodeRole;
use serde_json::Value;

#[test]
fn the_operator_key_parser_and_masternode_role_agree() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../dashd-masternode/tests/fixtures/am1-key/core-v24.json"
    ))
    .unwrap();
    for row in fixture["rows"].as_array().unwrap() {
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
        let inactive = matches!(parse(&args), Ok(None));
        assert_eq!(
            MasternodeRole::from_args(&args).active(),
            !inactive,
            "{}/{}",
            row["source"],
            row["name"]
        );
    }
}

/// Step 7c's `load` is `Some` exactly for a key, and every value it refuses is one the step-6
/// role reads as active, so init stops at 7c with Core's error.
// Core: init.cpp:2156-2167 at v24.0.0
#[test]
fn load_and_masternode_role_agree() {
    use dashd_masternode::active::{ActiveMnInitError, load};

    let cases = [
        ("unset", None, Some(false)),
        ("empty", Some(String::new()), Some(false)),
        ("valid", Some("01".repeat(32)), Some(true)),
        ("invalid hex", Some("zz".repeat(32)), None),
        ("wrong length", Some("01".repeat(31)), None),
        ("zero", Some("00".repeat(32)), None),
        ("above the group order", Some("ff".repeat(32)), None),
    ];
    for (name, value, loaded) in cases {
        let args = ArgsManager::new();
        Program::Dashd.setup_args(&args);
        if let Some(value) = value {
            args.force_set_arg("-masternodeblsprivkey", &value);
        }
        let active = MasternodeRole::from_args(&args).active();
        match load(&args) {
            Ok(got) => {
                assert_eq!(Some(got.is_some()), loaded, "{name}");
                assert_eq!(active, got.is_some(), "{name}");
            }
            Err(e) => {
                assert_eq!((e, loaded), (ActiveMnInitError::InvalidKey, None), "{name}");
                assert!(active, "{name}");
            }
        }
    }
}
