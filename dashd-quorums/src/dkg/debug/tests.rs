//! `CDKGDebugManager` (`l/debug.cpp:19-229`, v24.0.0): the status updates, the
//! `members.at(idx)` bound and `ToJson`'s field names at each detail level.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use dash_evo::types::ProTxHash;
use dash_llmq::dkg::session::{DkgDebugSink, MemberStatusBit, SessionStatusBit};
use dash_params::{ChainParamsBuilder, LlmqParams, LlmqType, Network, Release};
use dash_primitives::QuorumHash;
use serde_json::Value;

use super::{DebugJsonSource, DebugManager};
use crate::dkg::QuorumPhase;

const T: LlmqType = LlmqType::LlmqTest;

fn params() -> LlmqParams {
    let setup = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().expect("regtest");
    setup.consensus.llmq(T).copied().expect("regtest runs llmq_test")
}

fn manager() -> (DebugManager, Arc<AtomicI64>) {
    let now = Arc::new(AtomicI64::new(100));
    let clock = now.clone();
    let m = DebugManager::new([(T, params())], move || clock.load(Ordering::SeqCst));
    (m, now)
}

fn hash() -> QuorumHash {
    QuorumHash::from(std::array::from_fn::<u8, 32, _>(|i| i as u8))
}

struct Source {
    members: Option<Vec<ProTxHash>>,
}

impl DebugJsonSource for Source {
    fn has_llmq(&self, t: LlmqType) -> bool {
        t == T
    }
    fn members(&self, _: LlmqType, _: &QuorumHash) -> Option<Vec<ProTxHash>> {
        self.members.clone()
    }
    fn format_time(&self, time: i64) -> String {
        format!("t{time}")
    }
}

fn keys(v: &Value) -> Vec<String> {
    let mut keys: Vec<String> = v.as_object().expect("an object").keys().cloned().collect();
    keys.sort();
    keys
}

fn sorted(names: &[&str]) -> Vec<String> {
    let mut names: Vec<String> = names.iter().map(|s| (*s).to_owned()).collect();
    names.sort();
    names
}

const STATUS: [&str; 15] = [
    "llmqType",
    "quorumHash",
    "quorumHeight",
    "phase",
    "sentContributions",
    "sentComplaint",
    "sentJustification",
    "sentPrematureCommitment",
    "aborted",
    "badMembers",
    "weComplain",
    "receivedContributions",
    "receivedComplaints",
    "receivedJustifications",
    "receivedPrematureCommitments",
];

/// A session at height 48 with member 1 bad, member 2's contribution received and a sent
/// contribution, in phase Contribute.
fn populated() -> DebugManager {
    let (m, _) = manager();
    let mut sink = &m;
    sink.init_local_session_status(T, 0, &hash(), 48);
    sink.update_local_member_status(T, 0, 1, MemberStatusBit::Bad);
    sink.update_local_member_status(T, 0, 2, MemberStatusBit::ReceivedContribution);
    sink.update_local_session_status(T, 0, SessionStatusBit::SentContributions);
    m.mark_phase_advanced(T, 0, QuorumPhase::Contribute);
    m
}

#[test]
fn to_json_has_cores_field_names_at_each_detail_level() {
    let m = populated();
    let src = Source {
        members: Some(vec![ProTxHash::from([1; 32]), ProTxHash::from([2; 32])]),
    };

    let v = m.to_json(0, &src);
    assert_eq!(keys(&v), sorted(&["time", "timeStr", "session"]));
    assert_eq!(v["time"], 100);
    assert_eq!(v["timeStr"], "t100");
    let session = &v["session"][0];
    assert_eq!(keys(session), sorted(&["llmqType", "quorumIndex", "status"]));
    assert_eq!(session["llmqType"], "llmq_test");
    assert_eq!(session["quorumIndex"], 0);
    let status = &session["status"];
    assert_eq!(keys(status), sorted(&STATUS));
    assert_eq!(status["llmqType"], 100);
    let reversed: String = (0..32u8).rev().map(|b| format!("{b:02x}")).collect();
    assert_eq!(status["quorumHash"], reversed.as_str());
    assert_eq!(status["quorumHeight"], 48);
    assert_eq!(status["phase"], 2);
    assert_eq!(status["sentContributions"], true);
    assert_eq!(status["sentComplaint"], false);
    assert_eq!(status["badMembers"], 1);
    assert_eq!(status["receivedContributions"], 1);
    assert_eq!(status["weComplain"], 0);

    let status = &m.to_json(1, &src)["session"][0]["status"];
    assert_eq!(keys(status), sorted(&STATUS));
    assert_eq!(status["badMembers"], serde_json::json!([1]));
    assert_eq!(status["receivedContributions"], serde_json::json!([2]));

    let status = &m.to_json(2, &src)["session"][0]["status"];
    let mut with_all = STATUS.to_vec();
    with_all.push("allMembers");
    assert_eq!(keys(status), sorted(&with_all));
    assert_eq!(
        status["badMembers"],
        serde_json::json!([{"memberIndex": 1, "proTxHash": ProTxHash::from([2; 32]).to_string()}])
    );
    assert_eq!(status["receivedContributions"], serde_json::json!([{"memberIndex": 2}]));
    assert_eq!(status["allMembers"].as_array().map(Vec::len), Some(2));
}

#[test]
fn to_json_skips_unknown_types_and_empties_a_null_hash() {
    let (m, _) = manager();
    let mut sink = &m;
    sink.init_local_session_status(T, 0, &QuorumHash::default(), 48);
    let src = Source {
        members: None,
    };
    let v = m.to_json(0, &src);
    assert_eq!(v["session"][0]["status"], serde_json::json!({}));

    struct NoTypes;
    impl DebugJsonSource for NoTypes {
        fn has_llmq(&self, _: LlmqType) -> bool {
            false
        }
        fn members(&self, _: LlmqType, _: &QuorumHash) -> Option<Vec<ProTxHash>> {
            None
        }
        fn format_time(&self, time: i64) -> String {
            time.to_string()
        }
    }
    assert_eq!(m.to_json(0, &NoTypes)["session"], serde_json::json!([]));
}

fn ordered(v: &Value) -> Vec<&str> {
    v.as_object().expect("an object").keys().map(String::as_str).collect()
}

/// UniValue keeps `pushKV` order (`l/debug.cpp:36-108, 130-150`).
#[test]
fn to_json_keeps_cores_key_order() {
    let m = populated();
    let src = Source {
        members: Some(vec![ProTxHash::from([1; 32]), ProTxHash::from([2; 32])]),
    };
    let v = m.to_json(2, &src);
    assert_eq!(ordered(&v), ["time", "timeStr", "session"]);
    assert_eq!(ordered(&v["session"][0]), ["llmqType", "quorumIndex", "status"]);
    let mut status = STATUS.to_vec();
    status.push("allMembers");
    assert_eq!(ordered(&v["session"][0]["status"]), status);
    assert_eq!(ordered(&v["session"][0]["status"]["badMembers"][0]), ["memberIndex", "proTxHash"]);
}

#[test]
fn member_index_is_bounded_by_the_type_size() {
    let (m, _) = manager();
    m.update_member(T, 0, 1000, |_| true);
    let mut sink = &m;
    sink.init_local_session_status(T, 0, &hash(), 48);
    m.update_member(T, 0, 2, |s| {
        s.bad = true;
        true
    });
    let size = usize::from(params().size);
    let past = catch_unwind(AssertUnwindSafe(|| m.update_member(T, 0, size, |_| true)));
    assert!(past.is_err(), "members.at({size}) throws");
    assert!(m.session(T, 0).expect("a session").members[2].bad);
}

#[test]
fn time_moves_as_core_moves_it() {
    let (m, now) = manager();
    m.reset(T, 0);
    assert_eq!(m.time(), 0, "nothing to erase");
    let mut sink = &m;
    sink.init_local_session_status(T, 0, &hash(), 48);
    assert_eq!(m.time(), 0, "InitLocalSessionStatus leaves the time");
    now.store(200, Ordering::SeqCst);
    m.mark_phase_advanced(T, 0, QuorumPhase::Initialized);
    assert_eq!(m.time(), 200);
    now.store(300, Ordering::SeqCst);
    m.mark_phase_advanced(T, 0, QuorumPhase::Initialized);
    assert_eq!(m.time(), 200, "an unchanged phase");
    sink.add_local_member_complaint(T, 0, 1, 2);
    assert_eq!(m.time(), 300);
    now.store(400, Ordering::SeqCst);
    sink.add_local_member_complaint(T, 0, 1, 2);
    assert_eq!(m.time(), 300, "a complaint already counted");
    m.mark_aborted(T, 0);
    assert_eq!(m.time(), 400);
    assert!(m.session(T, 0).expect("a session").aborted);
    now.store(500, Ordering::SeqCst);
    m.reset(T, 0);
    assert_eq!(m.time(), 500);
    assert_eq!(m.session_count(), 0);
    m.mark_aborted(T, 0);
    assert_eq!(m.session_count(), 0, "MarkAborted needs a session");
}

#[test]
fn init_resets_a_session() {
    let m = populated();
    let mut sink = &m;
    sink.init_local_session_status(T, 0, &QuorumHash::from([9; 32]), 72);
    let s = m.session(T, 0).expect("a session");
    assert_eq!((s.phase, s.quorum_height, s.sent_contributions), (0, 72, false));
    assert_eq!(s.members.len(), usize::from(params().size));
    assert!(s.members.iter().all(|m| *m == super::MemberStatus::default()));
}
