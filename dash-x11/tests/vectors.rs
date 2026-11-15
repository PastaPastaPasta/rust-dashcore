//! Block hashes Dash Core asserts, checkpoints or ships as test data.
//!
//! - `known-hashes.json` (`contrib/extract-vectors.py`): the genesis hashes Core's
//!   chainparams.cpp asserts for mainnet, testnet and regtest, and every mainnet and testnet
//!   checkpoint, with the headers Core's reference nodes hold for them.
//! - `blockfilters.json`: Core's `src/test/data/blockfilters.json` (v24.0.0-rc.3, identical in
//!   v23.1.8), whose block hashes Dash Core computed with X11.

mod common;

use common::{display, implementations, unhex};
use dash_x11::{Implementation, Use};

#[test]
fn known_hashes() {
    let json: serde_json::Value =
        serde_json::from_str(include_str!("data/known-hashes.json")).unwrap();
    let entries: Vec<_> = json["genesis"]
        .as_array()
        .unwrap()
        .iter()
        .chain(json["checkpoints"].as_array().unwrap())
        .collect();
    assert_eq!(entries.len(), 3 + 58);
    for imp in implementations() {
        for e in &entries {
            let header = unhex(e["header"].as_str().unwrap());
            assert_eq!(header.len(), 80);
            assert_eq!(display(&imp.x11(&header)), e["hash"], "{e} ({})", imp.name());
        }
    }
}

#[test]
fn core_blockfilters() {
    let json: serde_json::Value =
        serde_json::from_str(include_str!("data/blockfilters.json")).unwrap();
    let rows = &json.as_array().unwrap()[1..];
    assert_eq!(rows.len(), 7);
    for imp in implementations() {
        for row in rows {
            let block = unhex(row[2].as_str().unwrap());
            assert_eq!(
                display(&imp.x11(&block[..80])),
                row[1],
                "height {} ({})",
                row[0],
                imp.name()
            );
        }
    }
}

#[test]
fn auto_and_free_function_agree() {
    let header = unhex(
        "010000000000000000000000000000000000000000000000000000000000000000000000c762a6567f3cc092\
         f0684bb62b7e00a84890b990f07cc71a6bb58d64b98e02e0022ddb52f0ff0f1ec23fb901",
    );
    let want = "00000ffd590b1485b3caadc19b22e6379c733355108f107a430458cdf3407ab6";
    assert_eq!(display(&dash_x11::x11(&header)), want);
    assert_eq!(display(&Implementation::auto().x11(&header)), want);
    assert_eq!(display(&Implementation::standard().x11(&header)), want);
    // On a healthy CPU `init` keeps every backend: its fallback must not hide a broken one.
    let sel = dash_x11::init();
    assert!(!sel.fell_back(), "{sel:?}");
    assert_eq!(sel.failed().count(), 0);
    assert_eq!(sel.implementation().name(), Implementation::detect(Use::ALL).name());
    assert_eq!(sel.preferred().name(), sel.implementation().name());
    assert_eq!(Implementation::auto().name(), sel.implementation().name());
    assert_eq!(dash_x11::selection(), Some(sel));
    assert_eq!(format!("{sel}"), format!("'{}'", sel.implementation().name()));
}
