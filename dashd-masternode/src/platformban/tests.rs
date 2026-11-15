use dash_params::{ChainParamsBuilder, Network, Release};
use dash_types::Numeric;
use serde_json::Value;

use super::*;

fn unhex<const N: usize>(s: &str) -> [u8; N] {
    assert_eq!(s.len(), 2 * N);
    std::array::from_fn(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
}

// Core: v24.0.0 net_processing.cpp:3906-3913; llmq/signhash.cpp:14-23; bls/bls.cpp:310-325.
#[test]
fn core_v24_net_processing_3906_3913() {
    let capture: Value =
        serde_json::from_str(include_str!("../../tests/fixtures/platformban/core-v24.json"))
            .unwrap();
    assert_eq!(capture["core_commit"], "938fba502824b9d850b7c710601456dc3d71603e");
    assert_eq!(capture["byte_order"], "stored");
    let rows = capture["vectors"].as_array().unwrap();
    assert_eq!(rows.len(), 26);
    for row in rows {
        let protx = ProTxHash::from(unhex(row["protx"].as_str().unwrap()));
        let height = i32::try_from(row["height"].as_i64().unwrap()).unwrap();
        let hash = QuorumHash::from(unhex(row["quorum_hash"].as_str().unwrap()));
        let bls = BlsCtx::from_legacy(row["legacy"].as_bool().unwrap());
        let public_key =
            BlsPkValue::from_canonical(&unhex(row["public_key"].as_str().unwrap()), bls.scheme())
                .unwrap();
        let sig =
            BlsSigValue::from_canonical(&unhex(row["signature"].as_str().unwrap()), bls.scheme());
        let sig = if row["case"] == "corrupted_signature" {
            sig.unwrap_or_else(BlsSigValue::invalid)
        } else {
            sig.unwrap()
        };
        let network = match row["llmq_type"].as_u64().unwrap() {
            4 => Network::Main,
            6 => Network::Testnet3,
            106 => Network::Regtest,
            other => panic!("{other}"),
        };
        let params = ChainParamsBuilder::new(network, Release::V24_0).build().unwrap();
        assert_eq!(
            u64::from(params.consensus.llmq_type_platform.to_base()),
            row["llmq_type"].as_u64().unwrap(),
            "{row}"
        );
        let (request, message) = ban_hashes(&protx, height);
        assert_eq!(request, unhex(row["request_id"].as_str().unwrap()), "{row}");
        assert_eq!(message, unhex(row["msg_hash"].as_str().unwrap()), "{row}");
        assert_eq!(
            sign_hash(params.consensus.llmq_type_platform, &hash, &request, &message),
            unhex(row["sign_hash"].as_str().unwrap()),
            "{row}"
        );
        let valid = row["valid"].as_bool().unwrap();
        assert_eq!(
            valid,
            matches!(
                row["case"].as_str().unwrap(),
                "valid" | "type_100_67" | "type_25_67" | "height"
            ),
            "{row}"
        );
        assert_eq!(
            verify_ban_sig(
                &params,
                &protx,
                height,
                BanQuorum {
                    quorum_hash: &hash,
                    public_key: &public_key
                },
                &sig,
                bls,
            ),
            if valid {
                BanSig::Valid
            } else {
                BanSig::Invalid
            },
            "{row}"
        );
    }
}

// Core: v24.0.0 net_processing.cpp:3913; bls/bls.cpp:310-325.
#[test]
fn core_v24_bls_310_325_verdicts() {
    assert_eq!(BanSig::from(BlsVerdict::Valid), BanSig::Valid);
    assert_eq!(BanSig::from(BlsVerdict::Invalid), BanSig::Invalid);
    assert_eq!(BanSig::from(BlsVerdict::Unsupported), BanSig::Unsupported);
}
