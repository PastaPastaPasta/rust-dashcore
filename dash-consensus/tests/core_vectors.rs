//! L0: the vectors Dash Core's own unit tests compute (O4, `test-vectors/core/<commit>/
//! transcribed/`): `pow_tests`, `subsidy_tests` and `block_reward_reallocation_tests`.

mod common;

use common::{
    VecChain, chain_params, flag, int, int32, payment_params, pow_params, subsidy_params, uint32,
};
use dash_consensus::payments::{
    MnRewardEra, masternode_payment, owner_payout_amounts, platform_share,
};
use dash_consensus::pow::get_next_work_required;
use dash_consensus::subsidy::{block_subsidy_inner, superblock_subsidy_inner};
use dash_types::Numeric;
use serde_json::Value;

fn records(file: &str) -> Vec<Value> {
    let text = dash_core_vectors::read_core_text(&format!("transcribed/{file}"));
    let mut doc: Value = serde_json::from_str(&text).unwrap();
    let Value::Array(records) = doc["records"].take() else {
        panic!("{file}: no records")
    };
    records
}

/// Core's `pow_tests/get_next_work`: 24 real mainnet headers ending at 123456, then
/// `GetNextWorkRequired` on mainnet and on a devnet (the slow-block rules).
#[test]
fn pow_tests_get_next_work() {
    let records = records("pow_tests.json");
    let headers = records.iter().find(|r| r["kind"] == "mainnet_headers").expect("headers");
    let window = &headers["headers_time_bits"];
    let len = u32::try_from(window.as_array().unwrap().len()).unwrap();
    let chain = VecChain::from_json(uint32(&headers["last_height"]) + 1 - len, window);

    let mut checked = 0;
    for record in records.iter().filter(|r| r["kind"] == "next_work") {
        let name = record["chain"].as_str().unwrap();
        let params = pow_params(&chain_params("v24.0", name));
        let new_time = uint32(&record["new_block_time"]);
        let core = uint32(&record["core_bits"]);
        assert_eq!(uint32(&record["expected_bits"]), core, "the vector disagrees with itself");
        let bits = get_next_work_required(&chain, chain.tip(), new_time, &params);
        assert_eq!(bits.to_base(), core, "{name} at nTime {new_time}");
        checked += 1;
    }
    assert_eq!(checked, 6);
}

/// Core's `subsidy_tests` (mainnet): the early-mainnet difficulty, all three formulas with their
/// clamps, the fixed V20 base, and the decline.
#[test]
fn subsidy_tests() {
    let params = subsidy_params(&chain_params("v24.0", "main"));
    let records = records("subsidy_tests.json");
    assert_eq!(records.len(), 12);
    for r in &records {
        let v20 = flag(&r["v20_active"]);
        let got =
            block_subsidy_inner(uint32(&r["prev_bits"]), int32(&r["prev_height"]), v20, &params);
        assert_eq!(got, Ok(int(&r["subsidy"])), "{r}");
    }
}

/// Core's `block_reward_reallocation_tests`: 71 checkpoints of a regtest chain through BRR, V20
/// and MN_RR, with the subsidy, the treasury part, the masternode payment and the block
/// template's coinbase value and masternode outputs.
#[test]
fn block_reward_reallocation_tests() {
    let cp = chain_params("v24.0", "regtest-brr");
    let (sub, pay) = (subsidy_params(&cp), payment_params(&cp));
    let records = records("block_reward_reallocation_tests.json");
    assert_eq!(records.len(), 71);
    for r in &records {
        let (height, bits) = (int32(&r["height"]), uint32(&r["prev_bits"]));
        let era = match int(&r["era"]) {
            0 => MnRewardEra::Classic,
            1 => MnRewardEra::CreditPool,
            2 => MnRewardEra::EvoReward,
            other => panic!("era {other}"),
        };
        let v20 = flag(&r["v20_active"]);
        assert_eq!(v20, era != MnRewardEra::Classic);
        let block_subsidy = int(&r["block_subsidy"]);

        // GetBlockSubsidyInner / GetSuperblockSubsidyInner(tip->nBits, tip->nHeight, v20)
        assert_eq!(block_subsidy_inner(bits, height, v20, &sub), Ok(block_subsidy), "{r}");
        let treasury = superblock_subsidy_inner(bits, height, v20, &sub);
        assert_eq!(treasury, Ok(int(&r["superblock_subsidy"])), "{r}");

        // GetMasternodePayment(tip->nHeight, block_subsidy, era)
        let payment = masternode_payment(height, block_subsidy, era, &pay);
        assert_eq!(payment, int(&r["masternode_payment"]), "{r}");

        // The template for the next block: no fees and no triggered superblock, so the coinbase
        // pays the subsidy. GetBlockTxOuts pays the platform share first in EvoReward, then the
        // single owner payout; the test masternode has no operator reward.
        assert_eq!(int(&r["template_coinbase_value_out"]), block_subsidy, "{r}");
        let mn_reward = masternode_payment(height + 1, block_subsidy, era, &pay);
        let mut expected = Vec::new();
        let mut owner_reward = mn_reward;
        if era == MnRewardEra::EvoReward {
            let share = platform_share(mn_reward).unwrap();
            expected.push(share);
            owner_reward -= share;
        }
        expected.extend(owner_payout_amounts(owner_reward, &[10_000]));
        let template: Vec<i64> =
            r["template_masternode_payments"].as_array().unwrap().iter().map(int).collect();
        assert_eq!(template, expected, "{r}");
    }
}
