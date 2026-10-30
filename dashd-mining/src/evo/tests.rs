use dash_primitives::{CoinbaseCommitment, MerkleRoot};

use super::*;

/// The CbTx payload dashd v24.0.0-rc.4 mined at 432 on a default no-masternode regtest chain
/// (`tests/data/mn0-evo-regtest.json`, chain "default"): version 3, both roots null, a null best
/// ChainLock and the first platform reward as the credit pool's balance.
const DASHD_CBTX_432: &str = concat!(
    "0300",                                                             // nVersion 3
    "b0010000",                                                         // nHeight 432
    "0000000000000000000000000000000000000000000000000000000000000000", // merkleRootMNList
    "0000000000000000000000000000000000000000000000000000000000000000", // merkleRootQuorums
    "00",                                                               // bestCLHeightDiff
    "000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
    "000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
    "442d3a0700000000", // creditPoolBalance 1.21253188
);

fn unhex(s: &str) -> Vec<u8> {
    dash_dml::testkit::replay::unhex(s).expect("hex")
}

/// The null best ChainLock signature is the 96 bytes dashd writes for a default `CBLSSignature`.
#[test]
fn null_chainlock_is_dashds() {
    let dashd = unhex(DASHD_CBTX_432);
    let decoded = CoinbaseCommitment::decode(&mut &dashd[..]).expect("dashd's CbTx");
    let signature = decoded.best_cl_signature.as_ref().expect("v3");
    assert_eq!(signature.to_bytes(), null_signature().to_bytes());

    let ours = CoinbaseCommitment {
        version: 3,
        height: 432,
        merkle_root_mn_list: MerkleRoot::default(),
        merkle_root_quorums: Some(MerkleRoot::default()),
        best_cl_height_diff: Some(0),
        best_cl_signature: Some(null_signature()),
        credit_pool_balance: Some(121_253_188),
        merkle_root_asset_unlocks: None,
    };
    let mut bytes = Vec::new();
    ours.encode(&mut bytes);
    assert_eq!(bytes, dashd);
}

/// `CCbTx::nVersion`: V24 counts only with V20; V20 over DIP0008; DIP0008 over version 1.
#[test]
fn cbtx_version_follows_cores_order() {
    let eras = |dip8, v20, v24| Eras {
        dip3: true,
        dip8,
        v20,
        mn_rr: false,
        v24,
    };
    let table = [
        ((false, false, false), 1),
        ((false, false, true), 1),
        ((true, false, false), 2),
        ((true, false, true), 2),
        ((true, true, false), 3),
        ((false, true, false), 3),
        ((true, true, true), 4),
        ((false, true, true), 4),
    ];
    for ((dip8, v20, v24), version) in table {
        assert_eq!(eras(dip8, v20, v24).cbtx_version(), version, "{dip8} {v20} {v24}");
    }
}
