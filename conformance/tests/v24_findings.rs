//! Snapshot of the library findings on Dash Core v24 payloads.
//!
//! The fixtures are blocks from regtest chains produced by Dash Core v24.0.0-rc.3's own
//! functional tests (`feature_asset_locks`, `feature_masternode_shares`,
//! `feature_masternode_payout_shares`, `feature_protx_version`), each with dashd's
//! `getblock 1` and `getblock 2` views. Every entry below is a known library bug, recorded in
//! `docs/node-program/research/BS2-conformance-results.md`. dashcore implements the v24 formats
//! (V24a), base-sdk its payloads (BS6a) and the stable txid (BS6b), so no bug remains. When a
//! fix lands in dashcore or base-sdk, the corresponding entries disappear and this snapshot
//! must be updated: that is the point (a fix is proven by the harness, and a regression shows
//! up here).

use std::collections::BTreeSet;

use dash_conformance::check::{BlockJob, Context, OracleBlock, check_block};
use dash_conformance::report::{Side, Stats};
use serde_json::Value;

fn findings(name: &str) -> BTreeSet<String> {
    let path = format!("{}/tests/data/{name}.json", env!("CARGO_MANIFEST_DIR"));
    let v: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let bytes = hex_conservative::decode_to_vec(v["hex"].as_str().unwrap()).unwrap();
    let oracle = OracleBlock::from_json(&v["getblock1"]).unwrap();
    let job = BlockJob {
        bytes: &bytes,
        height: Some(oracle.height),
        oracle: Some(&oracle),
        verbose: Some(&v["getblock2"]),
        ..Default::default()
    };
    let ctx = Context {
        net: "regtest".into(),
        source: "test",
    };
    let mut stats = Stats::default();
    check_block(&ctx, &job, &mut stats)
        .into_iter()
        .map(|m| {
            let side = match m.side {
                Side::Dashcore => "dashcore",
                Side::BaseSdk => "base-sdk",
                Side::Both => "both",
                Side::Undecided => "undecided",
                Side::Harness => "harness",
                Side::DashConsensus => "dash-consensus",
            };
            let tx = m.tx_index.map_or_else(|| "block".to_string(), |i| format!("tx{i}"));
            match m.field {
                Some(f) => format!("{tx} {} {side} {f}", m.check),
                None => format!("{tx} {} {side}", m.check),
            }
        })
        .collect()
}

fn assert_findings(name: &str, expected: &[&str]) {
    let actual = findings(name);
    let expected: BTreeSet<String> = expected.iter().map(|s| s.to_string()).collect();
    assert_eq!(actual, expected, "{name}: actual (left) vs recorded (right)");
}

#[test]
fn cbtx_v4() {
    // CbTx v4 (`merkleRootAssetUnlocks`) and asset locks v1 and v2.
    assert_findings("regtest-feature_asset_locks-750", &[]);
}

#[test]
fn asset_unlock_v2_stable_txid() {
    // Both libraries implement v24's stable asset-unlock txid (dashcore V24a, base-sdk BS6b).
    assert_findings("regtest-feature_asset_locks-969", &[]);
}

#[test]
fn pro_reg_tx_v3_shares() {
    // Both libraries match every field dashd prints for the shared registration (ExtAddr
    // netinfo, shares, early-dissolution terms, no top-level owner).
    assert_findings("regtest-feature_masternode_shares-273", &[]);
}

#[test]
fn pro_reg_tx_v3_payouts() {
    assert_findings("regtest-feature_masternode_payout_shares-121", &[]);
}

#[test]
fn pro_up_reg_tx_v3() {
    assert_findings("regtest-feature_masternode_payout_shares-124", &[]);
}

#[test]
fn pro_up_serv_tx_v3_agrees() {
    // Both libraries decode the ExtAddr ProUpServTx and agree with each other and with dashd.
    assert_findings("regtest-feature_masternode_payout_shares-125", &[]);
}

#[test]
fn new_special_types_10_to_12() {
    // ProDisTx (10), ProUpShareTx (11), ProUpSharedRegTx (12): both libraries decode them and
    // match dashd's `proDisTx`/`proUpShareTx`/`proUpSharedRegTx` objects field by field (a
    // field a library did not emit would be `verbose.missing`).
    for block in [327, 330, 335] {
        assert_findings(&format!("regtest-feature_masternode_shares-{block}"), &[]);
    }
}

#[test]
fn pre_v24_evo_protx_is_clean() {
    // v2 (BasicBLS) Evo ProRegTx/ProUpServTx before v24 activation: both libraries agree with
    // each other and with dashd.
    assert_findings("regtest-feature_protx_version-201", &[]);
}

#[test]
fn pro_up_reg_tx_v3_after_protx_upgrade() {
    assert_findings("regtest-feature_protx_version-384", &[]);
}
