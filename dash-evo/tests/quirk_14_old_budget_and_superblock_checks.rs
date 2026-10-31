//! Quirk 14: old-budget windows are unbounded under NoCheck; superblock payees are checked only
//! when synced and not ChainLocked (Core `src/masternode/payments.cpp:270-300,313-390` at
//! v24.0.0-rc.3).
//!
//! Provenance of `data/quirk-14-v24.0.json`: Core's `IsBlockValueValid` through
//! `oracle_block_value_payee` (dash-oracle-sys, pin v24.0) on the blocks of O2a-GV's payments
//! fixture (`dash-oracle-sys/tests/data/payments-v24.0.json.gz`): before, in and outside a mainnet
//! budget window, and mainnet's and testnet's superblocks, with their coinbases, triggers and node
//! states changed. `cargo run -p node-l1 --features oracle --example payments_quirk_vectors`
//! wrote it, and node-l1's `diff_block_value_payee` agrees with Core on every vector. Each holds
//! the governance answers Core gave (`IsSuperblockTriggered`, `GetSuperblockPayments`), which
//! the test serves through a `GovernanceView`.

use std::convert::Infallible;

use dash_consensus::Tx;
use dash_consensus::deployments::bip9_enabled;
use dash_evo::payments::{
    BlockValue, Ev3Branch, GovernanceView, PaymentsParams, SuperblockCheck, block_value_valid,
    invalid_superblock_error, valid_superblock,
};
use dash_params::{ChainParamsBuilder, ChainSetup, DeploymentPos, Network, Release};
use dash_primitives::{Transaction, TxOut, TxType};
use serde_json::Value;

const VECTORS: &str = include_str!("data/quirk-14-v24.0.json");

/// Core's governance answers, as recorded.
struct Recorded {
    loaded: bool,
    triggered: bool,
    payments: Option<Vec<TxOut>>,
}

impl GovernanceView for Recorded {
    type Error = Infallible;

    fn gov_valid(&mut self) -> Result<bool, Infallible> {
        Ok(self.loaded)
    }

    fn sb_triggered(&mut self, _: u32) -> Result<bool, Infallible> {
        Ok(self.triggered)
    }

    fn sb_payments(&mut self, _: u32) -> Result<Option<Vec<TxOut>>, Infallible> {
        Ok(self.payments.clone())
    }
}

fn params(kind: Network) -> ChainSetup {
    ChainParamsBuilder::new(kind, Release::V24_0).build().expect("Core's parameters")
}

fn outputs(v: &Value) -> Vec<TxOut> {
    let unhex = |s: &str| -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
            .collect()
    };
    v.as_array()
        .expect("outputs")
        .iter()
        .map(|o| TxOut {
            value: o[0].as_i64().expect("a value"),
            script_pubkey: unhex(o[1].as_str().expect("a script")).into(),
        })
        .collect()
}

/// Our value check with the vector's governance answers: `None` when valid, else the error.
fn value_error(v: &Value, chain: &ChainSetup, p: &PaymentsParams) -> Option<String> {
    let flag = |k: &str| v[k].as_bool().expect(k);
    let height = u32::try_from(v["height"].as_u64().expect("a height")).expect("a u32 height");
    let block_reward = v["block_reward"].as_i64().expect("a reward");
    let coinbase = Tx::new(Transaction {
        version: 3,
        tx_type: TxType::new(5),
        inputs: vec![],
        outputs: outputs(&v["outputs"]),
        lock_time: 0,
        extra_payload: vec![],
    });
    let superblock = outputs(&v["superblock"]);
    let mut gov = Recorded {
        loaded: flag("governance_loaded"),
        triggered: flag("triggered"),
        payments: (!superblock.is_empty()).then_some(superblock),
    };
    // ConnectBlock's check type (validation.cpp:2841-2843).
    let v24 = flag("v24") && bip9_enabled(chain, DeploymentPos::V24);
    let check = SuperblockCheck::for_block(flag("synced") && !flag("chainlocked"), v24);
    assert_eq!(format!("{check:?}"), v["check"].as_str().expect("a check"), "{}", v["name"]);

    match block_value_valid(&coinbase, height, block_reward, check, p, &mut gov).expect("an answer")
    {
        BlockValue::Valid => None,
        BlockValue::Invalid(why) => Some(why),
        BlockValue::Ev3(Ev3Branch::ValidSuperblock {
            block_reward,
            is_v24,
        }) => (!valid_superblock(&coinbase, height, block_reward, is_v24, p, &mut gov)
            .expect("an answer"))
        .then(|| invalid_superblock_error(height)),
        BlockValue::Ev3(other) => panic!("{}: {other:?}", v["name"]),
    }
}

#[test]
fn quirk_14_old_budget_and_superblock_checks() {
    let main = params(Network::Main);
    let test = params(Network::Testnet3);
    let pp = |c: &ChainSetup| PaymentsParams::from_chain_params(c).expect("payment parameters");
    let (main_p, test_p) = (pp(&main), pp(&test));
    let json: Value = serde_json::from_str(VECTORS).expect("the vectors");
    let vectors = json["vectors"].as_array().expect("vectors");

    let mut unbounded_window = 0;
    let mut unchecked_superblock = 0;
    for v in vectors {
        let (chain, p) = match v["chain"].as_str() {
            Some("main") => (&main, &main_p),
            Some("test") => (&test, &test_p),
            other => panic!("chain {other:?}"),
        };
        let ours = value_error(v, chain, p);
        let core = v["value_error"].as_str().expect("an error");
        assert_eq!(ours.is_none(), v["value_valid"].as_bool().expect("valid"), "{}", v["name"]);
        assert_eq!(ours.as_deref().unwrap_or(""), core, "{}", v["name"]);

        // The quirk's two halves, as Core shows them: a budget-window block paying more than the
        // reward, and a superblock paying a trigger it does not match, both accepted where no
        // superblock check is required.
        let name = v["name"].as_str().expect("a name");
        let accepted = ours.is_none() && v["check"] == "NoCheck";
        unbounded_window += usize::from(accepted && name.starts_with("budget-window"));
        unchecked_superblock += usize::from(
            accepted && name.contains("trigger a duff more") && name.ends_with("pays mined"),
        );
    }
    assert!(unbounded_window >= 4, "{unbounded_window}");
    assert!(unchecked_superblock >= 4, "{unchecked_superblock}");
}
