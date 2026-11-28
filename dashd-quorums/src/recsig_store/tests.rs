use std::path::Path;
use std::sync::Arc;

use dash_evo::locks::wire::RecoveredSig;
use dash_llmq::signing::db::RecSigDb;
use dash_llmq::signing::manager::SigningManager;
use dash_params::LlmqType;
use dash_types::Numeric;
use dashd_store::{RocksConfig, RocksEngine};
use serde_json::Value;

use super::*;

const T: LlmqType = LlmqType::LlmqTest;
const P: LlmqType = LlmqType::LlmqTestPlatform;

fn open(dir: &Path) -> KvRecSigStore<RocksEngine> {
    KvRecSigStore::new(RocksEngine::open(dir, &RECSIGDB_SCHEMA, &RocksConfig::default()).unwrap().0)
}

fn db(dir: &Path) -> RecSigDb<KvRecSigStore<RocksEngine>> {
    RecSigDb::new(open(dir), P)
}

fn sig(t: LlmqType, id: u8, msg: u8) -> RecoveredSig {
    RecoveredSig {
        llmq_type: t,
        quorum_hash: [0x51; 32].into(),
        id: [id; 32],
        msg_hash: [msg; 32],
        sig: [0x99; 96],
    }
}

fn rows<E: KvEngine>(store: &KvRecSigStore<E>) -> Vec<(Vec<u8>, Vec<u8>)> {
    store.db.iter_prefix(ROWS, &[]).map(Result::unwrap).collect()
}

/// A key as Core's `CDBWrapper` serializes a tuple led by a short string: its CompactSize length,
/// its bytes, then the fields.
fn key(prefix: &str, fields: &[&[u8]]) -> Vec<u8> {
    let mut out = vec![u8::try_from(prefix.len()).unwrap()];
    out.extend_from_slice(prefix.as_bytes());
    for field in fields {
        out.extend_from_slice(field);
    }
    out
}

fn wire(r: &RecoveredSig) -> Vec<u8> {
    [&[r.llmq_type.to_base()][..], r.quorum_hash.as_bytes(), &r.id, &r.msg_hash, &r.sig].concat()
}

/// Every row a recovered signature and a vote write, byte for byte, on RocksDB: Core's keys
/// (CompactSize-prefixed strings, the type byte, big-endian times) and values.
// Core: v24.0.2 src/llmq/signing.cpp:146-186 WriteRecoveredSig, :330-340 WriteVoteForId
#[test]
fn every_row_is_cores_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = db(dir.path());
    let (r, p) = (sig(T, 1, 2), sig(P, 3, 4));
    let now: u32 = 0x0102_0304;
    db.write(&r, i64::from(now)).unwrap();
    db.write(&p, i64::from(now)).unwrap();
    db.write_vote(T, &[5; 32], &[6; 32], i64::from(now)).unwrap();
    let (t, pt) = ([T.to_base()], [P.to_base()]);
    let be = now.to_be_bytes();
    let mut expected = vec![
        (key("rs_r", &[&t, &r.id]), wire(&r)),
        (key("rs_r", &[&t, &r.id, &r.msg_hash]), now.to_le_bytes().to_vec()),
        (key("rs_t", &[&be, &t, &r.id]), vec![1]),
        (key("rs_h", &[&r.hash()]), [&t[..], &r.id].concat()),
        (key("rs_s", &[&r.sign_hash()]), vec![1]),
        (key("rs_p", &[&p.hash()]), wire(&p)),
        (key("rs_m", &[&pt, &p.id, &p.msg_hash]), p.hash().to_vec()),
        (key("rs_u", &[&be, &p.hash()]), vec![1]),
        (key("rs_h", &[&p.hash()]), [&pt[..], &p.id].concat()),
        (key("rs_s", &[&p.sign_hash()]), vec![1]),
        (key("rs_v", &[&t, &[5; 32]]), vec![6; 32]),
        (key("rs_vt", &[&be, &t, &[5; 32]]), vec![1]),
    ];
    expected.sort();
    assert_eq!(rows(db.store()), expected);
}

/// Rows written through one engine are read back after it closes and the directory reopens.
#[test]
fn rows_survive_a_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let (r, p) = (sig(T, 1, 2), sig(P, 3, 4));
    {
        let mut db = db(dir.path());
        db.write(&r, 100).unwrap();
        db.write(&p, 100).unwrap();
        db.write_vote(T, &[5; 32], &[6; 32], 100).unwrap();
    }
    let db = db(dir.path());
    assert_eq!(db.get_by_id(T, &r.id).unwrap(), Some(r.clone()));
    assert_eq!(db.get(P, &p.id, &p.msg_hash).unwrap(), Some(p.clone()));
    assert_eq!(db.get_by_hash(&r.hash()).unwrap(), Some(r.clone()));
    assert!(db.has_for_session(&p.sign_hash()).unwrap());
    assert_eq!(db.vote_for(T, &[5; 32]).unwrap(), Some([6; 32]));
}

/// Truncation removes the by-id rows and keeps the hash, session and time rows; a later cleanup
/// then removes the time row and nothing else, as Core's `RemoveRecoveredSig` finds no by-id row.
// Core: v24.0.2 src/llmq/signing.cpp:188-234, :266-316
#[test]
fn truncate_keeps_the_hash_and_session_rows() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = db(dir.path());
    let r = sig(T, 1, 2);
    db.write(&r, 100).unwrap();
    db.truncate(T, &r.id).unwrap();
    let t = [T.to_base()];
    let kept: Vec<_> = rows(db.store()).into_iter().map(|(k, _)| k).collect();
    let mut expected = vec![
        key("rs_t", &[&100u32.to_be_bytes(), &t, &r.id]),
        key("rs_h", &[&r.hash()]),
        key("rs_s", &[&r.sign_hash()]),
    ];
    expected.sort();
    assert_eq!(kept, expected);
    assert!(db.has_for_hash(&r.hash()).unwrap() && !db.has_for_id(T, &r.id).unwrap());
    db.cleanup_old(10, 200).unwrap();
    let kept: Vec<_> = rows(db.store()).into_iter().map(|(k, _)| k).collect();
    assert_eq!(kept, expected[..2].to_vec());
}

/// Cleanup erases rows strictly older than `now - max_age`, Platform rows by their own time index,
/// and leaves the newer rows and the votes; vote cleanup has the same strict boundary.
// Core: v24.0.2 src/llmq/signing.cpp:236-316 CleanupOld*, :342-381 CleanupOldVotes
#[test]
fn cleanup_erases_old_rows_and_keeps_new_ones() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = db(dir.path());
    let (old, new, old_p) = (sig(T, 1, 2), sig(T, 3, 4), sig(P, 5, 6));
    db.write(&old, 100).unwrap();
    db.write(&old_p, 100).unwrap();
    db.write(&new, 101).unwrap();
    db.write_vote(T, &[7; 32], &[8; 32], 100).unwrap();
    db.write_vote(T, &[9; 32], &[8; 32], 101).unwrap();
    db.cleanup_old(10, 111).unwrap();
    assert!(!db.has_for_hash(&old.hash()).unwrap() && !db.has_for_hash(&old_p.hash()).unwrap());
    assert!(!db.has_for_session(&old_p.sign_hash()).unwrap());
    assert_eq!(db.get_by_hash(&new.hash()).unwrap(), Some(new.clone()));
    db.cleanup_old_votes(10, 111).unwrap();
    assert_eq!(db.vote_for(T, &[7; 32]).unwrap(), None);
    assert_eq!(db.vote_for(T, &[9; 32]).unwrap(), Some([8; 32]));
    let t = [T.to_base()];
    let left: Vec<_> = rows(db.store()).into_iter().map(|(k, _)| k).collect();
    let mut expected = vec![
        key("rs_r", &[&t, &new.id]),
        key("rs_r", &[&t, &new.id, &new.msg_hash]),
        key("rs_t", &[&101u32.to_be_bytes(), &t, &new.id]),
        key("rs_h", &[&new.hash()]),
        key("rs_s", &[&new.sign_hash()]),
        key("rs_v", &[&t, &[9; 32]]),
        key("rs_vt", &[&101u32.to_be_bytes(), &t, &[9; 32]]),
    ];
    expected.sort();
    assert_eq!(left, expected);
}

/// The store, counting its writes.
struct Counted(KvRecSigStore<RocksEngine>, usize);

impl RecSigKv for Counted {
    type Error = StoreError;

    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        self.0.get(key)
    }

    fn scan_from(&self, start: &[u8]) -> Result<RecSigScan<'_, StoreError>, StoreError> {
        self.0.scan_from(start)
    }

    fn write(&mut self, batch: RecSigBatch) -> Result<(), StoreError> {
        self.1 += 1;
        self.0.write(batch)
    }
}

/// A cleanup of more rows than one 16 MiB batch estimate holds writes the batch when it reaches
/// the estimate and starts a new one; every old row is gone at the end.
// Core: v24.0.2 src/llmq/signing.cpp:295-313
#[test]
fn cleanup_writes_its_batch_at_16_mib() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    let n: u32 = 100_000;
    let mut batch = RecSigBatch::default();
    let t = [T.to_base()];
    for i in 0..n {
        let mut id = [0; 32];
        id[..4].copy_from_slice(&i.to_le_bytes());
        let r = RecoveredSig {
            id,
            ..sig(T, 0, 2)
        };
        batch.put(key("rs_r", &[&t, &id]), wire(&r));
        batch.put(key("rs_r", &[&t, &id, &r.msg_hash]), 100u32.to_le_bytes().to_vec());
        batch.put(key("rs_t", &[&100u32.to_be_bytes(), &t, &id]), vec![1]);
        batch.put(key("rs_h", &[&r.hash()]), [&t[..], &id].concat());
        batch.put(key("rs_s", &[&r.sign_hash()]), vec![1]);
    }
    store.write(batch).unwrap();
    let mut db = RecSigDb::new(Counted(store, 0), P);
    db.cleanup_old(10, 200).unwrap();
    let Counted(store, writes) = db.into_inner();
    assert_eq!(writes, 3, "the Platform pass's batch, one flush at 16 MiB, then the rest");
    assert!(rows(&store).is_empty());
}

/// A scan reads the rows as of its creation: a write while it is open does not show in it.
#[test]
fn a_scan_is_a_view_as_of_its_creation() {
    fn check<E: KvEngine>(store: &KvRecSigStore<E>) {
        let mut batch = RecSigBatch::default();
        batch.put(vec![1], vec![1]);
        batch.put(vec![3], vec![3]);
        store.write(batch).unwrap();
        let mut scan = store.scan_from(&[2]).unwrap();
        let mut batch = RecSigBatch::default();
        batch.put(vec![4], vec![4]);
        batch.delete(vec![3]);
        store.write(batch).unwrap();
        assert_eq!(scan.next().map(Result::unwrap), Some((vec![3], vec![3])));
        assert!(scan.next().is_none());
        drop(scan);
        let after: Vec<_> = store.scan_from(&[]).unwrap().map(Result::unwrap).collect();
        assert_eq!(after, [(vec![1], vec![1]), (vec![4], vec![4])]);
    }
    let dir = tempfile::tempdir().unwrap();
    check(&open(dir.path()));
    check(&KvRecSigStore::in_memory());
}

fn lane_c() -> Value {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-vectors/lane-c/regtest-v24.0.0.json");
    serde_json::from_slice(&std::fs::read(path).expect("the lane-c fixture")).expect("JSON")
}

/// A hash in RPC display order (`GetHex`), as stored bytes.
fn uint256(v: &Value) -> [u8; 32] {
    let mut out: [u8; 32] = hex(v).try_into().expect("32 bytes");
    out.reverse();
    out
}

fn hex(v: &Value) -> Vec<u8> {
    let s = v.as_str().expect("a hex string");
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex")).collect()
}

/// Lane (c)'s node1 (`-maxrecsigsage=60`) on the persistent store, with Core's mock times: the
/// recorded signature written at its phase's held time is there at +60 s and gone after the
/// cleanup at +61 s; the isolated vote is there at +60 s and gone after the vote cleanup, when a
/// vote on another message is taken. The store is reopened between steps, and each step's
/// manager is new, so no cleanup is throttled.
// Core: v24.0.2 src/llmq/signing.cpp:266-316, :342-381, :580-611, :630-634
#[test]
fn lane_c_node1_maxrecsigsage_60_on_rocksdb() {
    let fixture = lane_c();
    let cleanup = &fixture["cleanup"];
    assert_eq!(cleanup["node"], 1);
    let request = fixture["requests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == cleanup["request_id"])
        .expect("the cleanup request");
    let rec = &request["recsig"];
    let sig = RecoveredSig {
        llmq_type: LlmqType::from_base(u8::try_from(rec["llmqType"].as_u64().unwrap()).unwrap()),
        quorum_hash: uint256(&rec["quorumHash"]).into(),
        id: uint256(&rec["id"]),
        msg_hash: uint256(&rec["msgHash"]),
        sig: hex(&rec["sig"]).try_into().expect("96 bytes"),
    };
    assert_eq!(sig.hash(), uint256(&request["recsig_hash"]));
    assert_eq!(sig.sign_hash(), uint256(&request["signHash"]));
    let written = cleanup["write_mocktime"].as_i64().unwrap();
    let vote_time = cleanup["vote_time"].as_i64().unwrap();
    let vote_id = uint256(&cleanup["vote_only_id"]);
    let [first, second] = [0, 1].map(|i| uint256(&cleanup["vote_messages"][i]));
    let phase = |n: u64| {
        let phases = fixture["control_phases"].as_array().unwrap();
        let phase = phases.iter().find(|p| p["phase"] == n).expect("the phase");
        phase["held_mocktime"].as_i64().unwrap()
    };
    assert_eq!(phase(55), written + 60);
    assert_eq!((phase(56), phase(57)), (vote_time, vote_time + 67));
    let observed = |phase: u64, id: &[u8; 32]| {
        let obs = fixture["observations"].as_array().unwrap().iter().find(|o| {
            o["method"] == "quorum hasrecsig"
                && o["node"] == 1
                && o["phase"] == phase
                && uint256(&o["params"][1]) == *id
        });
        obs.expect("the observation")["result"].as_bool().unwrap()
    };

    let dir = tempfile::tempdir().unwrap();
    let manager = || SigningManager::new(open(dir.path()), P, 60);
    let (t, id, msg) = (sig.llmq_type, sig.id, sig.msg_hash);
    let event = manager().process_recovered(Arc::new(sig.clone()), false, written).unwrap();
    assert!(event.is_some(), "stored");
    let mut node1 = manager();
    assert!(node1.cleanup((written + 60) * 1000).unwrap());
    assert!(node1.db().has_recovered_sig(t, &id, &msg).unwrap());
    assert!(observed(55, &id));
    drop(node1);
    let mut node1 = manager();
    assert!(node1.cleanup((written + 61) * 1000).unwrap());
    assert!(!node1.db().has_recovered_sig(t, &id, &msg).unwrap());
    assert!(!node1.already_have(&sig.hash()).unwrap());
    assert!(!observed(56, &id) && !observed(58, &id));
    drop(node1);

    let mut node1 = manager();
    node1.write_vote(t, &vote_id, &first, vote_time).unwrap();
    drop(node1);
    let mut node1 = manager();
    assert!(node1.cleanup((vote_time + 60) * 1000).unwrap());
    assert_eq!(node1.vote_for(t, &vote_id).unwrap(), Some(first));
    drop(node1);
    let mut node1 = manager();
    assert!(node1.cleanup(phase(57) * 1000).unwrap());
    assert_eq!(node1.vote_for(t, &vote_id).unwrap(), None);
    node1.write_vote(t, &vote_id, &second, phase(57)).unwrap();
    assert!(!observed(57, &vote_id));
    drop(node1);
    assert_eq!(manager().vote_for(t, &vote_id).unwrap(), Some(second));
}
