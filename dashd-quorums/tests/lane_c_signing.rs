//! Lane (c) through the signing API: Core v24.0.0's recorded Platform same-id/different-message
//! requests and node1's two isolated vote-only calls, replayed as `QuorumSigning` and
//! `RecoveredSigIntake` calls on one runtime per node. Quorum membership comes from the recorded
//! submissions; every compared answer is a recorded Core answer.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use dash_clock::MockClock;
use dash_evo::locks::wire::RecoveredSig;
use dash_evo::types::ProTxHash;
use dash_llmq::dkg::bls::BlsPkValue;
use dash_llmq::sigenv::SignRequest;
use dash_llmq::sigenv::fakes::{FakeLookup, FakeQuorum};
use dash_llmq::signing::api::{QuorumSigning, RecoveredSigIntake};
use dash_llmq::signing::db::{RecSigBatch, RecSigKv, RecSigOp, RecSigScan};
use dash_llmq::signing::manager::SigningManager;
use dash_p2p_core::Inventory;
use dash_params::{ChainParamsBuilder, ChainSetup, LlmqType, Network, Release};
use dash_primitives::{QuorumHash, TxHash};
use dash_types::Numeric as _;
use dashd_quorums::LocalMasternode;
use dashd_quorums::signing::{
    RecSigNotify, SignQueue, SigningHandles, SigningLookup, SigningNet, SigningRuntime,
};
use serde_json::Value;

/// Node1's `-maxrecsigsage` (test-vectors/lane-c/PROVENANCE.md:67).
const NODE1_MAX_AGE: i64 = 60;

#[derive(Default)]
struct Mem(BTreeMap<Vec<u8>, Vec<u8>>);

impl RecSigKv for Mem {
    type Error = std::convert::Infallible;

    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Self::Error> {
        Ok(self.0.get(key).cloned())
    }

    fn scan_from(&self, start: &[u8]) -> Result<RecSigScan<'_, Self::Error>, Self::Error> {
        Ok(Box::new(self.0.range(start.to_vec()..).map(|(k, v)| Ok((k.clone(), v.clone())))))
    }

    fn write(&mut self, batch: RecSigBatch) -> Result<(), Self::Error> {
        for op in batch.ops {
            match op {
                RecSigOp::Put(k, v) => {
                    self.0.insert(k, v);
                }
                RecSigOp::Delete(k) => {
                    self.0.remove(&k);
                }
            }
        }
        Ok(())
    }
}

struct Lookup(FakeLookup);

impl dash_llmq::sigenv::QuorumLookup for Lookup {
    type Quorum = FakeQuorum;
    type Error = std::convert::Infallible;

    fn get_quorum(
        &self,
        t: LlmqType,
        h: &QuorumHash,
    ) -> Result<Option<Arc<FakeQuorum>>, Self::Error> {
        self.0.get_quorum(t, h)
    }

    fn is_quorum_active(&self, t: LlmqType, h: &QuorumHash) -> Result<bool, Self::Error> {
        self.0.is_quorum_active(t, h)
    }

    fn select_quorum_for_signing(
        &self,
        t: LlmqType,
        id: &[u8; 32],
        height: i32,
        offset: i32,
    ) -> Result<Option<Arc<FakeQuorum>>, Self::Error> {
        self.0.select_quorum_for_signing(t, id, height, offset)
    }
}

impl SigningLookup for Lookup {
    fn base_height(&self, _: &FakeQuorum) -> i32 {
        unreachable!("lane (c) requests name their quorum")
    }
}

struct Node(Option<ProTxHash>);

impl SigningNet for Node {
    fn misbehaving(&self, peer: i64, _: i32) {
        panic!("peer {peer} scored");
    }
    fn relay_inv(&self, _: &Inventory) {}
    fn relay_transaction(&self, _: &TxHash) {}
}

impl RecSigNotify for Node {
    fn notify_recovered_sig(&self, _: Arc<RecoveredSig>, _: bool) {}
}

impl SignQueue<FakeQuorum> for Node {
    fn force_reannouncement(&self, _: &Arc<FakeQuorum>, _: LlmqType, _: &[u8; 32], _: &[u8; 32]) {}
    fn async_sign(&self, _: Arc<FakeQuorum>, _: [u8; 32], _: [u8; 32]) {}
}

impl LocalMasternode for Node {
    fn pro_tx_hash(&self) -> Option<ProTxHash> {
        self.0
    }
}

fn regtest() -> ChainSetup {
    ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap()
}

fn fixture() -> Value {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-vectors/lane-c/regtest-v24.0.0.json");
    serde_json::from_slice(&std::fs::read(path).expect("lane (c) fixture")).expect("json")
}

/// An RPC display-order (`GetHex`) hash as stored bytes.
fn hash(v: &Value) -> [u8; 32] {
    let mut out: [u8; 32] = unhex(v.as_str().expect("hash")).try_into().expect("32 bytes");
    out.reverse();
    out
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex")).collect()
}

fn display(bytes: [u8; 32]) -> String {
    bytes.iter().rev().map(|b| format!("{b:02x}")).collect()
}

fn recsig(request: &Value) -> Arc<RecoveredSig> {
    let r = &request["recsig"];
    let sig = RecoveredSig {
        llmq_type: LlmqType::from_base(r["llmqType"].as_u64().expect("type") as u8),
        quorum_hash: hash(&r["quorumHash"]).into(),
        id: hash(&r["id"]),
        msg_hash: hash(&r["msgHash"]),
        sig: unhex(r["sig"].as_str().expect("sig")).try_into().expect("96 bytes"),
    };
    assert_eq!(display(sig.hash()), request["recsig_hash"].as_str().unwrap());
    assert_eq!(display(sig.sign_hash()), request["signHash"].as_str().unwrap());
    Arc::new(sig)
}

fn pro_tx(fixture: &Value, node: &str) -> Option<ProTxHash> {
    let v = &fixture["signers"][node]["proTxHash"];
    (!v.is_null()).then(|| ProTxHash::from(hash(v)))
}

type Runtime = SigningRuntime<Mem, Lookup>;

fn runtime(
    pro_tx: Option<ProTxHash>,
    quorum: Arc<FakeQuorum>,
    max_age: i64,
    clock: &Arc<MockClock>,
) -> Runtime {
    let platform = regtest().consensus.llmq_type_platform;
    let mut lookup = FakeLookup::default();
    let key = (quorum.llmq_type.to_base(), quorum.quorum_hash);
    lookup.active.insert(key);
    lookup.quorums.insert(key, quorum);
    let node = Arc::new(Node(pro_tx));
    let handles = SigningHandles {
        net: node.clone(),
        notify: node.clone(),
        masternode: node.clone(),
        clock: clock.clone(),
    };
    let rt = SigningRuntime::new(
        SigningManager::new(Mem::default(), platform, max_age),
        Arc::new(Lookup(lookup)),
        Arc::new(regtest()),
        handles,
    );
    assert!(rt.set_sign_queue(node).is_ok());
    rt
}

fn quorum(t: LlmqType, quorum_hash: [u8; 32], members: Vec<ProTxHash>) -> Arc<FakeQuorum> {
    Arc::new(FakeQuorum {
        llmq_type: t,
        quorum_hash: quorum_hash.into(),
        params: *regtest().consensus.llmq(t).expect("regtest type"),
        valid_members: (0..members.len() as u16).collect(),
        members,
        has_verification_vector: false,
        quorum_public_key: BlsPkValue::invalid(),
        pk_shares: BTreeMap::new(),
        sk_share: None,
    })
}

/// The RPC's `quorum sign … submit`: Platform may sign a different message (rpc/quorums.cpp:545).
fn sign(rt: &Runtime, t: LlmqType, id: [u8; 32], msg: [u8; 32], qh: [u8; 32]) -> bool {
    let platform = t.to_base() == regtest().consensus.llmq_type_platform.to_base();
    let req = SignRequest {
        llmq_type: t,
        id,
        msg_hash: msg,
        quorum_hash: Some(qh.into()),
        allow_resign: false,
        allow_diff_msg: platform,
    };
    rt.async_sign_if_member(&req).expect("sign")
}

#[test]
fn platform_same_id_pairs_match_core_on_every_node() {
    let f = fixture();
    let requests = f["requests"].as_array().unwrap();
    let firsts = requests.iter().filter(|r| r["label"] == "platform_same_id_first");
    let mut pairs = 0;
    for first in firsts {
        let second = requests
            .iter()
            .find(|r| r["label"] == "platform_same_id_second" && r["id"] == first["id"])
            .expect("the same id's second request");
        let t = LlmqType::from_base(first["type"].as_u64().unwrap() as u8);
        assert_eq!(t, LlmqType::LlmqTestPlatform);
        let (id, qh) = (hash(&first["id"]), hash(&first["quorumHash"]));
        assert_eq!(hash(&second["quorumHash"]), qh);
        let (msg1, msg2) = (hash(&first["msgHash"]), hash(&second["msgHash"]));
        assert_ne!(msg1, msg2);
        let (rs1, rs2) = (recsig(first), recsig(second));
        let submits = |r: &Value| -> BTreeMap<String, bool> {
            r["submissions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| (s["node"].to_string(), s["result"].as_bool().unwrap()))
                .collect()
        };
        let (submit1, submit2) = (submits(first), submits(second));
        let members: BTreeSet<_> =
            submit1.keys().chain(submit2.keys()).filter_map(|n| pro_tx(&f, n)).collect();
        let q = quorum(t, qh, members.into_iter().collect());

        for node in f["signers"].as_object().unwrap().keys() {
            let clock = Arc::new(MockClock::at(first["mocktime"].as_i64().unwrap()));
            let rt = runtime(pro_tx(&f, node), q.clone(), 604_800, &clock);
            if let Some(&expected) = submit1.get(node) {
                assert_eq!(sign(&rt, t, id, msg1, qh), expected, "node {node} first sign");
            }
            assert!(rt.process_recovered_sig(rs1.clone(), true).unwrap());
            assert_eq!(rt.has_recovered_sig(t, &id, &msg1).unwrap(), first["hasrecsig"][node]);
            assert_eq!(rt.is_conflicting(t, &id, &msg1).unwrap(), first["isconflicting"][node]);

            clock.set_mocktime(second["mocktime"].as_i64().unwrap());
            if let Some(&expected) = submit2.get(node) {
                assert_eq!(sign(&rt, t, id, msg2, qh), expected, "node {node} second sign");
                assert_eq!(rt.get_vote_for_id(t, &id).unwrap(), Some(msg2));
            }
            assert!(rt.process_recovered_sig(rs2.clone(), true).unwrap());
            assert_eq!(rt.has_recovered_sig(t, &id, &msg2).unwrap(), second["hasrecsig"][node]);
            assert_eq!(rt.is_conflicting(t, &id, &msg2).unwrap(), second["isconflicting"][node]);
            assert!(rt.has_recovered_sig(t, &id, &msg1).unwrap());
            assert_eq!(rt.get_recovered_sig(t, &id, &msg2).unwrap().as_ref(), Some(&*rs2));
        }
        pairs += 1;
    }
    assert_eq!(pairs, 2);
}

#[test]
fn node1_vote_only_calls_match_core_across_vote_expiry() {
    let f = fixture();
    let cleanup = &f["cleanup"];
    let node = cleanup["node"].to_string();
    let vote_only = hash(&cleanup["vote_only_id"]);
    let calls: Vec<&Value> = f["observations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|o| o["sign_exact_reason"] == "isolated_after_barrier")
        .collect();
    assert_eq!(calls.len(), 2);
    let held = |phase: &Value| -> i64 {
        f["control_phases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["phase"] == *phase)
            .expect("control phase")["held_mocktime"]
            .as_i64()
            .unwrap()
    };
    let t = LlmqType::from_base(calls[0]["params"][0].as_u64().unwrap() as u8);
    let qh = hash(&calls[0]["params"][3]);
    let me = pro_tx(&f, &node).expect("node1 is a masternode");
    let clock = Arc::new(MockClock::at(held(&calls[0]["phase"])));
    assert_eq!(clock.mocktime(), cleanup["vote_time"].as_i64().unwrap());
    let rt = runtime(Some(me), quorum(t, qh, vec![me]), NODE1_MAX_AGE, &clock);

    for (i, call) in calls.iter().enumerate() {
        assert_eq!(call["node"].to_string(), node);
        assert_eq!(hash(&call["params"][1]), vote_only);
        let msg = hash(&call["params"][2]);
        assert_eq!(display(msg), cleanup["vote_messages"][i].as_str().unwrap());
        if i == 1 {
            clock.set_mocktime(held(&call["phase"]));
            assert!(!sign(&rt, t, vote_only, msg, qh), "the unexpired vote refuses");
            assert!(rt.cleanup().unwrap());
        }
        assert_eq!(sign(&rt, t, vote_only, msg, qh), call["result"].as_bool().unwrap());
        assert_eq!(rt.get_vote_for_id(t, &vote_only).unwrap(), Some(msg));
    }
}
