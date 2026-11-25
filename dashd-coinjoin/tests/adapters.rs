use std::sync::Arc;

use dash_args::{ArgsManager, Program};
use dash_coinjoin::queue::{MixingCounters, MixingMeta};
use dash_dml::testkit::dmn_with;
use dash_dml::{Dmn, MnType, ProTxHash};
use dash_num::Hash256;
use dash_pkc::bls::BlsSchemeKind;
use dash_primitives::TxHash;
use dashd_coinjoin::adapters::{ActiveMnAdapter, MetaManMixing};
use dashd_coinjoin::env::ActiveMn;
use dashd_masternode::active::key::OperatorPubKey;
use dashd_masternode::active::state::{LocalFacts, MnLookup, TipCtx};
use dashd_masternode::active::{ActiveMasternode, OperatorKey, parse};
use dashd_masternode::meta::MasternodeMetaMan;
use proptest::prelude::*;
use serde_json::Value;

const KEY_FIXTURE: &str =
    include_str!("../../dashd-masternode/tests/fixtures/am1-key/core-v24.json");
const MESSAGE_START: [u8; 4] = [0xfc, 0xc1, 0xb7, 0xdc];

#[derive(Clone, Debug)]
enum Op {
    Allow(u8),
    Disallow(u8),
    Threshold(u8, i32),
    Valid(u8),
}

fn mn_bytes(n: u8) -> [u8; 32] {
    let mut b = [0u8; 32];
    b[0] = n;
    b[31] = 0xA0 | n;
    b
}

fn hash(n: u8) -> TxHash {
    TxHash::from(mn_bytes(n))
}

fn pro(n: u8) -> ProTxHash {
    ProTxHash::from(mn_bytes(n))
}

fn op() -> impl Strategy<Value = Op> {
    let mn = 1..4u8;
    prop_oneof![
        1 => mn.clone().prop_map(Op::Allow),
        3 => mn.clone().prop_map(Op::Disallow),
        2 => (mn.clone(), 0..40i32).prop_map(|(n, count)| Op::Threshold(n, count)),
        1 => mn.prop_map(Op::Valid),
    ]
}

fn apply(meta: &mut impl MixingMeta, op: &Op) -> Option<bool> {
    match *op {
        Op::Allow(n) => {
            meta.allow_mixing(&hash(n));
            None
        }
        Op::Disallow(n) => {
            meta.disallow_mixing(&hash(n));
            None
        }
        Op::Threshold(n, count) => Some(meta.is_mixing_threshold_exceeded(&hash(n), count)),
        Op::Valid(n) => Some(meta.is_valid_for_mixing_txes(&hash(n))),
    }
}

proptest! {
    // Core: masternode/meta.cpp:93-128 at v24.0.0
    #[test]
    fn meta_man_mixing_answers_like_mixing_counters(ops in prop::collection::vec(op(), 0..80)) {
        let man = Arc::new(MasternodeMetaMan::new());
        let mut adapter = MetaManMixing(Arc::clone(&man));
        let mut counters = MixingCounters::default();
        for op in &ops {
            prop_assert_eq!(apply(&mut adapter, op), apply(&mut counters, op), "{:?}", op);
        }
        let store = man.snapshot();
        prop_assert_eq!(store.dsq_count, counters.dsq_count);
        prop_assert_eq!(store.infos.len(), counters.masternodes.len());
        for n in 1..4u8 {
            let info = man.info(&pro(n));
            let expected = counters
                .masternodes
                .get(&hash(n))
                .map_or((0, 0), |mn| (mn.last_dsq, mn.mixing_tx_count));
            prop_assert_eq!((info.last_dsq, info.mixing_tx_count), expected, "mn {}", n);
        }
    }
}

// Core: masternode/meta.cpp:25-36, :110-122 at v24.0.0
#[test]
fn counters_changed_through_the_adapter_survive_store_and_load() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(dashd_masternode::meta::FILE_NAME);
    let mut adapter = MetaManMixing(Arc::new(MasternodeMetaMan::new()));
    adapter.allow_mixing(&hash(1));
    adapter.allow_mixing(&hash(2));
    adapter.allow_mixing(&hash(1));
    for _ in 0..7 {
        adapter.disallow_mixing(&hash(2));
    }
    adapter.disallow_mixing(&hash(3));
    assert!(adapter.0.store(&path, MESSAGE_START));

    let loaded = MetaManMixing(Arc::new(MasternodeMetaMan::new()));
    assert!(loaded.0.load_cache(&path, MESSAGE_START, true));
    assert_eq!(loaded.0.snapshot().dsq_count, 3);
    for (n, last_dsq, mixing_tx_count) in [(1, 3, 0), (2, 2, 7), (3, 0, 1)] {
        let info = loaded.0.info(&pro(n));
        assert_eq!((info.last_dsq, info.mixing_tx_count), (last_dsq, mixing_tx_count), "mn {n}");
    }
    assert_eq!(loaded.0.snapshot(), adapter.0.snapshot());
    for n in 1..=4 {
        for count in [0, 5, 10, 15, 20] {
            assert_eq!(
                loaded.is_mixing_threshold_exceeded(&hash(n), count),
                adapter.is_mixing_threshold_exceeded(&hash(n), count),
                "mn {n}, count {count}"
            );
        }
        assert_eq!(
            loaded.is_valid_for_mixing_txes(&hash(n)),
            adapter.is_valid_for_mixing_txes(&hash(n))
        );
    }
    assert!(!loaded.is_valid_for_mixing_txes(&hash(2)));
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap()).collect()
}

fn key_for(row: &Value) -> OperatorKey {
    assert_eq!(row["source"], "command-line");
    let args = ArgsManager::new();
    Program::Dashd.setup_args(&args);
    let argv: Vec<&str> = std::iter::once("dashd")
        .chain(row["argv"].as_array().unwrap().iter().map(|arg| arg.as_str().unwrap()))
        .collect();
    args.parse_parameters(&argv).unwrap();
    parse(&args).unwrap().unwrap()
}

// Core: active/masternode.cpp:296-303 at v24.0.0
#[test]
fn sign_basic_matches_the_operator_key_on_recorded_core_vectors() {
    let fixture: Value = serde_json::from_str(KEY_FIXTURE).unwrap();
    let rows = fixture["signatures"]["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 4);
    for row in rows {
        let network_key = fixture["network_keys"]
            .as_array()
            .unwrap()
            .iter()
            .find(|key| key["index"] == row["network_index"])
            .unwrap();
        let key = key_for(network_key);
        let active = Arc::new(ActiveMasternode::new(key_for(network_key)));
        let hash: [u8; 32] = unhex(row["hash_bytes"].as_str().unwrap()).try_into().unwrap();
        let signed = active.sign_basic(&hash);
        assert_eq!(signed, key.sign_basic(&hash));
        assert_eq!(hex(&signed), row["sign_basic"]);
        assert_eq!(ActiveMnAdapter(active).sign_basic(&Hash256::from(hash)), signed);
    }
}

struct OneMn(Arc<Dmn>);

struct NoMn;

impl MnLookup for NoMn {
    fn by_operator_key(&self, _: &OperatorPubKey) -> Option<Arc<Dmn>> {
        None
    }
    fn is_valid(&self, _: &[u8; 32]) -> bool {
        false
    }
    fn is_pose_banned(&self, _: &[u8; 32]) -> bool {
        false
    }
    fn get(&self, _: &[u8; 32]) -> Option<Arc<Dmn>> {
        None
    }
}

impl MnLookup for OneMn {
    fn by_operator_key(&self, key: &OperatorPubKey) -> Option<Arc<Dmn>> {
        (self.0.cold.pub_key_operator.get() == *key).then(|| Arc::clone(&self.0))
    }
    fn is_valid(&self, _: &[u8; 32]) -> bool {
        true
    }
    fn is_pose_banned(&self, _: &[u8; 32]) -> bool {
        false
    }
    fn get(&self, pro: &[u8; 32]) -> Option<Arc<Dmn>> {
        (self.0.pro_tx_hash.as_bytes() == pro).then(|| Arc::clone(&self.0))
    }
}

// Core: active/masternode.h:67-68; active/masternode.cpp:168-171 at v24.0.0
#[test]
fn outpoint_is_null_until_ready_then_the_collateral() {
    let args = ArgsManager::new();
    args.force_set_arg("-masternodeblsprivkey", &"01".repeat(32));
    let active = Arc::new(ActiveMasternode::new(parse(&args).unwrap().unwrap()));
    let adapter = ActiveMnAdapter(Arc::clone(&active));
    assert!(active.outpoint().is_null());
    assert!(adapter.outpoint().is_null());
    assert_eq!(adapter.pro_tx_hash(), TxHash::from([0; 32]));

    let dmn = dmn_with(1, MnType::Regular, |_, c| {
        c.version = 2;
        c.pub_key_operator =
            dash_dml::OperatorKey::from_value(active.pubkey(), BlsSchemeKind::Basic);
    });
    let tip = TipCtx {
        height: 120,
        dip3_enforced: true,
        require_routable: false,
    };
    let local = LocalFacts {
        listen: true,
        require_routable: false,
        local_toward_v4: None,
        loopback_listen: dash_dml::netinfo::primary(&dmn.cold.net_info),
        peers: Vec::new(),
        valid: Vec::new(),
        self_connect_ok: true,
    };
    active.init(&tip, &OneMn(Arc::clone(&dmn)), &local);
    assert_eq!(active.state_string(), "READY");
    assert!(!dmn.collateral.is_null());
    assert_eq!(active.outpoint(), dmn.collateral);
    assert_eq!(adapter.outpoint(), dmn.collateral);
    assert_eq!(adapter.pro_tx_hash(), TxHash::from(*dmn.pro_tx_hash.as_bytes()));

    let removed = TipCtx {
        height: 121,
        ..tip
    };
    active.updated_block_tip(&removed, Ok((&OneMn(Arc::clone(&dmn)), &NoMn)), &local).unwrap();
    assert_ne!(active.state_string(), "READY");
    assert!(adapter.outpoint().is_null());
    assert_eq!(adapter.pro_tx_hash(), TxHash::from([0; 32]));
}
