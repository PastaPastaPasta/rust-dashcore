//! The txid reference used without an oracle (blk mode) follows Dash Core's rule, including
//! v24's stable asset-unlock txid, so RPC and blk mode judge the libraries' txids alike.

use dash_conformance::check::{BlockJob, Context, OracleBlock, check_block, core_txid};
use dash_conformance::report::Stats;
use dash_conformance::split::split_block;
use dashcore::hashes::{Hash, sha256d};
use serde_json::Value;

const FIXTURES: &[&str] = &[
    "testnet-7000",
    "testnet-7090",
    "regtest-feature_asset_locks-750",
    "regtest-feature_asset_locks-969",
    "regtest-feature_masternode_payout_shares-121",
    "regtest-feature_masternode_payout_shares-124",
    "regtest-feature_masternode_payout_shares-125",
    "regtest-feature_masternode_shares-273",
    "regtest-feature_masternode_shares-327",
    "regtest-feature_masternode_shares-330",
    "regtest-feature_masternode_shares-335",
    "regtest-feature_protx_version-201",
    "regtest-feature_protx_version-384",
];

fn load(name: &str) -> (Vec<u8>, OracleBlock) {
    let path = format!("{}/tests/data/{name}.json", env!("CARGO_MANIFEST_DIR"));
    let v: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let bytes = hex_conservative::decode_to_vec(v["hex"].as_str().unwrap()).unwrap();
    (bytes, OracleBlock::from_json(&v["getblock1"]).unwrap())
}

#[test]
fn reference_txid_matches_dashd_on_every_fixture() {
    let mut stable = 0;
    for name in FIXTURES {
        let (bytes, oracle) = load(name);
        let spans = split_block(&bytes).unwrap().txs;
        assert_eq!(spans.len(), oracle.txids.len(), "{name}");
        for (i, span) in spans.iter().enumerate() {
            let raw = &bytes[span.range.clone()];
            assert_eq!(core_txid(raw, span), oracle.txids[i], "{name} tx{i}");
            stable += usize::from(core_txid(raw, span) != sha256d::Hash::hash(raw).to_byte_array());
        }
    }
    assert_eq!(stable, 1, "the v24 asset unlock of feature_asset_locks-969");
}

#[test]
fn rpc_and_blk_mode_judge_txids_alike() {
    // Review r1 finding 6: without an oracle the reference was sha256d of the bytes, which
    // blessed both libraries' wrong txid for a v2 asset unlock.
    let ctx = Context {
        net: "regtest".into(),
        source: "test",
    };
    let (bytes, oracle) = load("regtest-feature_asset_locks-969");
    let txid_findings = |job: &BlockJob<'_>| {
        let mut found: Vec<_> = check_block(&ctx, job, &mut Stats::default())
            .into_iter()
            .filter(|m| m.check.starts_with("tx.txid"))
            .map(|m| (m.check, m.tx_index, m.side, m.expected))
            .collect();
        found.sort_by_key(|f| f.1);
        found
    };
    let rpc = BlockJob {
        bytes: &bytes,
        height: Some(oracle.height),
        oracle: Some(&oracle),
        ..Default::default()
    };
    let blk = BlockJob {
        bytes: &bytes,
        height: Some(oracle.height),
        expected_hash: Some(oracle.hash),
        ..Default::default()
    };
    // dashcore (V24a) and base-sdk (BS6b) both implement the stable txid.
    assert_eq!(txid_findings(&rpc), vec![], "RPC mode");
    assert_eq!(txid_findings(&blk), vec![], "blk mode");
}
