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
