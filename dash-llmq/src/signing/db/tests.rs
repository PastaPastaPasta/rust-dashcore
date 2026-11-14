use super::*;
use proptest::prelude::*;
use std::collections::{BTreeMap, BTreeSet};

const T: LlmqType = LlmqType::LlmqTest;
const P: LlmqType = LlmqType::LlmqTestPlatform;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Error {
    Get,
    Open,
    Advance,
    Write,
}

#[derive(Default)]
struct Mem {
    rows: BTreeMap<Vec<u8>, Vec<u8>>,
    fail_get: bool,
    fail_open: bool,
    fail_after: Option<usize>,
    fail_write: bool,
    writes: Vec<RecSigBatch>,
}

impl RecSigKv for Mem {
    type Error = Error;

    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Self::Error> {
        if self.fail_get {
            return Err(Error::Get);
        }
        Ok(self.rows.get(key).cloned())
    }

    fn scan_from(&self, start: &[u8]) -> Result<RecSigScan<'_, Self::Error>, Self::Error> {
        if self.fail_open {
            return Err(Error::Open);
        }
        let rows = self.rows.range(start.to_vec()..).enumerate().map(|(i, (k, v))| {
            if self.fail_after.is_some_and(|n| i >= n) {
                Err(Error::Advance)
            } else {
                Ok((k.clone(), v.clone()))
            }
        });
        Ok(Box::new(rows))
    }

    fn write(&mut self, batch: RecSigBatch) -> Result<(), Self::Error> {
        if self.fail_write {
            return Err(Error::Write);
        }
        for op in &batch.ops {
            match op {
                RecSigOp::Put(key, value) => {
                    self.rows.insert(key.clone(), value.clone());
                }
                RecSigOp::Delete(key) => {
                    self.rows.remove(key);
                }
            }
        }
        self.writes.push(batch);
        Ok(())
    }
}

fn sig(t: LlmqType, id: u8, msg: u8, quorum: u8) -> RecoveredSig {
    RecoveredSig {
        llmq_type: t,
        quorum_hash: [quorum; 32].into(),
        id: [id; 32],
        msg_hash: [msg; 32],
        sig: [quorum; 96],
    }
}

fn db() -> RecSigDb<Mem> {
    RecSigDb::new(Mem::default(), P)
}
fn sample() -> RecoveredSig {
    sig(T, 1, 2, 3)
}

#[test]
fn has_recovered_sig() {
    let mut db = db();
    let r = sample();
    assert!(!db.has_recovered_sig(T, &r.id, &r.msg_hash).unwrap());
    db.write(&r, 100).unwrap();
    assert!(db.has_recovered_sig(T, &r.id, &r.msg_hash).unwrap());
    assert!(!db.has_recovered_sig(T, &r.id, &[4; 32]).unwrap());
    assert!(!db.has_recovered_sig(P, &r.id, &r.msg_hash).unwrap());
}

#[test]
fn has_for_id() {
    let mut db = db();
    let r = sample();
    assert!(!db.has_for_id(T, &r.id).unwrap());
    db.write(&r, 100).unwrap();
    assert!(db.has_for_id(T, &r.id).unwrap());
    db.write(&sig(P, 1, 2, 3), 100).unwrap();
    assert!(!db.has_for_id(P, &r.id).unwrap());
}

#[test]
fn has_for_session() {
    let mut db = db();
    let r = sample();
    assert!(!db.has_for_session(&r.sign_hash()).unwrap());
    db.write(&r, 100).unwrap();
    assert!(db.has_for_session(&r.sign_hash()).unwrap());
    assert!(!db.has_for_session(&sig(T, 1, 4, 3).sign_hash()).unwrap());
}

#[test]
fn has_for_hash() {
    let mut db = db();
    let r = sample();
    assert!(!db.has_for_hash(&r.hash()).unwrap());
    db.write(&r, 100).unwrap();
    assert!(db.has_for_hash(&r.hash()).unwrap());
    assert!(!db.has_for_hash(&[0; 32]).unwrap());
}

#[test]
fn get_by_hash() {
    let mut db = db();
    let r = sample();
    assert_eq!(db.get_by_hash(&r.hash()).unwrap(), None);
    db.write(&r, 100).unwrap();
    assert_eq!(db.get_by_hash(&r.hash()).unwrap(), Some(r.clone()));
    let p = sig(P, 1, 2, 3);
    db.write(&p, 100).unwrap();
    assert_eq!(db.get_by_hash(&p.hash()).unwrap(), Some(p));
    db.store_mut().rows.insert(keys::recovered_platform(&r.hash()), vec![0]);
    assert_eq!(db.get_by_hash(&r.hash()).unwrap(), Some(r));
}

#[test]
fn get_by_id() {
    let mut db = db();
    let r = sample();
    assert_eq!(db.get_by_id(T, &r.id).unwrap(), None);
    db.write(&r, 100).unwrap();
    assert_eq!(db.get_by_id(T, &r.id).unwrap(), Some(r));
    db.write(&sig(P, 1, 2, 3), 100).unwrap();
    assert_eq!(db.get_by_id(P, &[1; 32]).unwrap(), None);
}

#[test]
fn get() {
    let mut db = db();
    let r = sample();
    assert_eq!(db.get(T, &r.id, &r.msg_hash).unwrap(), None);
    db.write(&r, 100).unwrap();
    assert_eq!(db.get(T, &r.id, &r.msg_hash).unwrap(), Some(r.clone()));
    assert_eq!(db.get(T, &r.id, &[4; 32]).unwrap(), None);
    let p = sig(P, 1, 2, 3);
    db.write(&p, 100).unwrap();
    assert_eq!(db.get(P, &p.id, &p.msg_hash).unwrap(), Some(p));
}

#[test]
fn write_uses_core_record_and_value_bytes_without_erasing_votes() {
    let mut db = db();
    let r = sample();
    db.write_vote(T, &r.id, &r.msg_hash, 50).unwrap();
    db.write(&r, 0x01020304).unwrap();
    let rows = &db.store().rows;
    let expected = [vec![100], vec![3; 32], vec![1; 32], vec![2; 32], vec![3; 96]].concat();
    assert_eq!(rows[&keys::recovered(T, &r.id)], expected);
    assert_eq!(rows[&keys::recovered_message(T, &r.id, &r.msg_hash)], [4, 3, 2, 1]);
    assert_eq!(rows[&keys::recovered_hash(&r.hash())], [vec![100], vec![1; 32]].concat());
    assert_eq!(rows[&keys::recovered_session(&r.sign_hash())], [1]);
    assert_eq!(db.vote_for(T, &r.id).unwrap(), Some(r.msg_hash));
}

#[test]
fn truncate_retains_hash_session_and_time_indexes_even_after_cleanup() {
    let mut db = db();
    let r = sample();
    db.write(&r, 10).unwrap();
    db.truncate(T, &r.id).unwrap();
    assert!(!db.has_for_id(T, &r.id).unwrap());
    assert!(!db.has_recovered_sig(T, &r.id, &r.msg_hash).unwrap());
    assert!(db.has_for_hash(&r.hash()).unwrap());
    assert!(db.has_for_session(&r.sign_hash()).unwrap());
    assert_eq!(db.get_by_hash(&r.hash()).unwrap(), None);
    assert!(db.store().rows.contains_key(&keys::recovered_time(10, T, &r.id)));
    db.cleanup_old(60, 100).unwrap();
    assert!(db.has_for_hash(&r.hash()).unwrap());
    assert!(db.has_for_session(&r.sign_hash()).unwrap());
    assert!(!db.store().rows.contains_key(&keys::recovered_time(10, T, &r.id)));
    let p = sig(P, 1, 2, 3);
    db.write(&p, 10).unwrap();
    db.truncate(P, &p.id).unwrap();
    assert_eq!(db.get(P, &p.id, &p.msg_hash).unwrap(), Some(p));
}

#[test]
fn remove_recovered_sig_flag_combinations_keep_time_key() {
    for delete_hash in [false, true] {
        let mut db = db();
        let r = sample();
        db.write(&r, 10).unwrap();
        let mut batch = RecSigBatch::default();
        db.remove_recovered_sig(&mut batch, T, &r.id, delete_hash).unwrap();
        db.kv.write(batch).unwrap();
        assert!(!db.has_for_id(T, &r.id).unwrap());
        assert_eq!(db.has_for_hash(&r.hash()).unwrap(), !delete_hash);
        assert_eq!(db.has_for_session(&r.sign_hash()).unwrap(), !delete_hash);
        assert!(db.store().rows.contains_key(&keys::recovered_time(10, T, &r.id)));
    }
}

#[test]
fn batch_flush_threshold_is_core_16_mib() {
    assert_eq!(BATCH_FLUSH_BYTES, 16 * 1024 * 1024);
}

#[test]
fn cleanup_old_strict_boundary_and_platform_first() {
    let mut db = db();
    for t in [T, P] {
        for time in [39, 40, 41] {
            db.write(&sig(t, time, time, 1), time as i64).unwrap();
        }
    }
    db.cleanup_old(60, 100).unwrap();
    for t in [T, P] {
        for time in [39, 40, 41] {
            let r = sig(t, time, time, 1);
            assert_eq!(db.has_recovered_sig(t, &r.id, &r.msg_hash).unwrap(), time >= 40);
            assert_eq!(db.has_for_hash(&r.hash()).unwrap(), time >= 40);
            assert_eq!(db.has_for_session(&r.sign_hash()).unwrap(), time >= 40);
        }
    }
    let writes = &db.store().writes;
    let platform_batch = &writes[writes.len() - 2];
    let regular_batch = &writes[writes.len() - 1];
    assert!(matches!(&platform_batch.ops[0], RecSigOp::Delete(k) if k.starts_with(b"\x04rs_p")));
    assert!(matches!(&regular_batch.ops[0], RecSigOp::Delete(k) if k.starts_with(b"\x04rs_r")));
}

#[test]
fn platform_messages_and_legacy_by_id_records() {
    for p in [P, LlmqType::Llmq100_67, LlmqType::Llmq25_67, LlmqType::LlmqDevnetPlatform] {
        let mut db = RecSigDb::new(Mem::default(), p);
        for msg in [1, 2, 3] {
            db.write(&sig(p, 1, msg, msg), 100).unwrap();
        }
        for msg in [1, 2, 3] {
            let r = sig(p, 1, msg, msg);
            assert!(db.has_recovered_sig(p, &r.id, &r.msg_hash).unwrap());
            assert_eq!(db.get(p, &r.id, &r.msg_hash).unwrap(), Some(r));
        }
        assert!(!db.has_for_id(p, &[1; 32]).unwrap());
        assert_eq!(db.get_by_id(p, &[1; 32]).unwrap(), None);
    }
    let r = sig(P, 2, 3, 4);
    let mut legacy = RecSigDb::new(Mem::default(), T);
    legacy.write(&r, 10).unwrap();
    let legacy_store = legacy.into_inner();
    let legacy = RecSigDb::new(legacy_store, P);
    assert!(legacy.has_for_id(P, &r.id).unwrap());
    assert!(legacy.has_recovered_sig(P, &r.id, &r.msg_hash).unwrap());
    assert_eq!(legacy.get(P, &r.id, &r.msg_hash).unwrap(), Some(r));
}

#[test]
fn has_voted() {
    let mut db = db();
    assert!(!db.has_voted(T, &[1; 32]).unwrap());
    db.write_vote(T, &[1; 32], &[2; 32], 10).unwrap();
    assert!(db.has_voted(T, &[1; 32]).unwrap());
    assert!(!db.has_voted(P, &[1; 32]).unwrap());
}

#[test]
fn vote_for() {
    let mut db = db();
    assert_eq!(db.vote_for(T, &[1; 32]).unwrap(), None);
    db.write_vote(T, &[1; 32], &[2; 32], 10).unwrap();
    assert_eq!(db.vote_for(T, &[1; 32]).unwrap(), Some([2; 32]));
}

#[test]
fn write_vote_keeps_prior_time_indexes() {
    let mut db = db();
    db.write_vote(T, &[1; 32], &[2; 32], 10).unwrap();
    db.write_vote(T, &[1; 32], &[3; 32], 20).unwrap();
    assert_eq!(db.vote_for(T, &[1; 32]).unwrap(), Some([3; 32]));
    assert!(db.store().rows.contains_key(&keys::vote_time(10, T, &[1; 32])));
    assert!(db.store().rows.contains_key(&keys::vote_time(20, T, &[1; 32])));
    db.cleanup_old_votes(60, 71).unwrap();
    assert_eq!(db.vote_for(T, &[1; 32]).unwrap(), None);
    assert!(db.store().rows.contains_key(&keys::vote_time(20, T, &[1; 32])));
}

#[test]
fn cleanup_old_votes_strict_boundary() {
    let mut db = db();
    for time in [39, 40, 41] {
        db.write_vote(T, &[time; 32], &[2; 32], time as i64).unwrap();
    }
    db.cleanup_old_votes(60, 100).unwrap();
    for time in [39, 40, 41] {
        assert_eq!(db.has_voted(T, &[time; 32]).unwrap(), time >= 40);
    }
}

#[test]
fn storage_errors_propagate_and_failed_batches_are_atomic() {
    let mut db = db();
    let r = sample();
    db.write(&r, 10).unwrap();
    db.kv.fail_get = true;
    assert_eq!(db.get_by_id(T, &r.id), Err(Error::Get));
    assert_eq!(db.has_for_hash(&r.hash()), Err(Error::Get));
    db.kv.fail_get = false;
    db.kv.fail_open = true;
    assert_eq!(db.cleanup_old(0, 100), Err(Error::Open));
    db.kv.fail_open = false;
    for platform in [false, true] {
        let mut db = RecSigDb::new(Mem::default(), P);
        let t = if platform {
            P
        } else {
            T
        };
        for n in [1, 2] {
            db.write(&sig(t, n, n, n), 10).unwrap();
        }
        db.write_vote(T, &[1; 32], &[2; 32], 10).unwrap();
        db.write_vote(T, &[2; 32], &[3; 32], 10).unwrap();
        let before = db.kv.rows.clone();
        db.kv.fail_after = Some(1);
        assert_eq!(db.cleanup_old(0, 100), Err(Error::Advance));
        assert_eq!(db.kv.rows, before);
        assert_eq!(db.cleanup_old_votes(0, 100), Err(Error::Advance));
        assert_eq!(db.kv.rows, before);
        db.kv.fail_after = None;
        db.kv.fail_write = true;
        assert_eq!(db.write(&sig(T, 3, 3, 3), 10), Err(Error::Write));
        assert_eq!(db.kv.rows, before);
    }
}

#[test]
fn malformed_values_and_iterator_stop_match_core_reads() {
    let mut db = db();
    let r = sample();
    db.kv.rows.insert(keys::recovered(T, &r.id), vec![0]);
    assert!(db.has_for_id(T, &r.id).unwrap());
    assert_eq!(db.get_by_id(T, &r.id).unwrap(), None);
    db.write(&r, 10).unwrap();
    // GetKey fails on this short time key: later records must be left alone.
    db.kv.rows.insert(b"\x04rs_t\0\0\0\x01".to_vec(), vec![1]);
    db.cleanup_old(0, 100).unwrap();
    assert_eq!(db.get_by_id(T, &r.id).unwrap(), Some(r));
}

// An independent logical transcription of signing.cpp:146-381. These maps model Core's
// records and indexes, including obsolete indexes left by overwrites and truncation;
// they do not call the DB implementation or its key codecs.
#[derive(Default)]
struct CoreModel {
    records: BTreeMap<(u8, [u8; 32]), RecoveredSig>,
    messages: BTreeSet<(u8, [u8; 32], [u8; 32])>,
    times: BTreeSet<(u32, u8, [u8; 32])>,
    hashes: BTreeMap<[u8; 32], (u8, [u8; 32])>,
    sessions: BTreeSet<[u8; 32]>,
    platform: BTreeMap<[u8; 32], RecoveredSig>,
    platform_messages: BTreeMap<(u8, [u8; 32], [u8; 32]), [u8; 32]>,
    platform_times: BTreeSet<(u32, [u8; 32])>,
    votes: BTreeMap<(u8, [u8; 32]), [u8; 32]>,
    vote_times: BTreeSet<(u32, u8, [u8; 32])>,
}

impl CoreModel {
    // Core: v24.0.0 src/llmq/signing.cpp:146-186 WriteRecoveredSig
    fn write(&mut self, r: &RecoveredSig, now: u32) {
        let t = r.llmq_type.to_base();
        if r.llmq_type == P {
            self.platform.insert(r.hash(), r.clone());
            self.platform_messages.insert((t, r.id, r.msg_hash), r.hash());
            self.platform_times.insert((now, r.hash()));
        } else {
            self.records.insert((t, r.id), r.clone());
            self.messages.insert((t, r.id, r.msg_hash));
            self.times.insert((now, t, r.id));
        }
        self.hashes.insert(r.hash(), (t, r.id));
        self.sessions.insert(r.sign_hash());
    }

    // Core: v24.0.0 src/llmq/signing.cpp:188-224 RemoveRecoveredSig
    fn remove(&mut self, t: u8, id: [u8; 32], delete_hash: bool) {
        if let Some(r) = self.records.remove(&(t, id)) {
            self.messages.remove(&(r.llmq_type.to_base(), r.id, r.msg_hash));
            if delete_hash {
                self.hashes.remove(&r.hash());
                self.sessions.remove(&r.sign_hash());
            }
        }
    }

    // Core: v24.0.0 src/llmq/signing.cpp:236-316 CleanupOldRecoveredSigs
    fn cleanup(&mut self, end: u32) {
        let expired: Vec<_> =
            self.platform_times.iter().copied().filter(|(at, _)| *at < end).collect();
        for (at, hash) in expired {
            if let Some(r) = self.platform.remove(&hash) {
                self.platform_messages.remove(&(r.llmq_type.to_base(), r.id, r.msg_hash));
                self.hashes.remove(&hash);
                self.sessions.remove(&r.sign_hash());
            }
            self.platform_times.remove(&(at, hash));
        }
        let expired: Vec<_> = self.times.iter().copied().filter(|(at, _, _)| *at < end).collect();
        for (_, t, id) in &expired {
            self.remove(*t, *id, true);
        }
        for index in expired {
            self.times.remove(&index);
        }
    }

    // Core: v24.0.0 src/llmq/signing.cpp:342-381 CleanupOldVotes
    fn cleanup_votes(&mut self, end: u32) {
        let expired: Vec<_> =
            self.vote_times.iter().copied().filter(|(at, _, _)| *at < end).collect();
        for (at, t, id) in expired {
            self.votes.remove(&(t, id));
            self.vote_times.remove(&(at, t, id));
        }
    }

    // Core: v24.0.0 src/llmq/signing.cpp:114-126 GetRecoveredSigByHash
    fn by_hash(&self, hash: &[u8; 32]) -> Option<RecoveredSig> {
        self.platform
            .get(hash)
            .cloned()
            .or_else(|| self.hashes.get(hash).and_then(|k| self.records.get(k)).cloned())
    }

    fn check(&self, db: &RecSigDb<Mem>, probes: &[RecoveredSig]) {
        for r in probes {
            let k = (r.llmq_type.to_base(), r.id);
            let m = (k.0, r.id, r.msg_hash);
            assert_eq!(db.has_for_id(r.llmq_type, &r.id).unwrap(), self.records.contains_key(&k));
            assert_eq!(db.get_by_id(r.llmq_type, &r.id).unwrap(), self.records.get(&k).cloned());
            assert_eq!(db.has_for_hash(&r.hash()).unwrap(), self.hashes.contains_key(&r.hash()));
            assert_eq!(
                db.has_for_session(&r.sign_hash()).unwrap(),
                self.sessions.contains(&r.sign_hash())
            );
            assert_eq!(db.get_by_hash(&r.hash()).unwrap(), self.by_hash(&r.hash()));
            assert_eq!(
                db.has_recovered_sig(r.llmq_type, &r.id, &r.msg_hash).unwrap(),
                self.messages.contains(&m)
                    || (r.llmq_type == P && self.platform_messages.contains_key(&m))
            );
            let result = if r.llmq_type == P && self.platform_messages.contains_key(&m) {
                self.by_hash(&self.platform_messages[&m])
            } else {
                self.records.get(&k).filter(|stored| stored.msg_hash == r.msg_hash).cloned()
            };
            assert_eq!(db.get(r.llmq_type, &r.id, &r.msg_hash).unwrap(), result);
            assert_eq!(db.has_voted(r.llmq_type, &r.id).unwrap(), self.votes.contains_key(&k));
            assert_eq!(db.vote_for(r.llmq_type, &r.id).unwrap(), self.votes.get(&k).copied());
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, failure_persistence: None, .. ProptestConfig::default() })]
    #[test]
    fn random_sequences_match_core_model(ops in prop::collection::vec((0u8..6, any::<bool>(), 1u8..4, 1u8..4, 1u8..3, 0u32..150, 0u32..70), 1..100)) {
        let mut db = db(); let mut model = CoreModel::default();
        let probes: Vec<_> = [T,P].into_iter().flat_map(|t| (1..4).flat_map(move |id| (1..4).flat_map(move |msg| (1..3).map(move |q| sig(t,id,msg,q))))).collect();
        for (op, platform, id, msg, q, now, age) in ops {
            let t = if platform { P } else { T }; let r = sig(t,id,msg,q);
            match op {
                0 | 1 => { db.write(&r, now as i64).unwrap(); model.write(&r, now); }
                2 => { db.truncate(t, &r.id).unwrap(); model.remove(t.to_base(), r.id, false); }
                3 => { db.cleanup_old(age as i64, now as i64).unwrap(); model.cleanup(now.wrapping_sub(age)); }
                4 => { db.write_vote(t, &r.id, &r.msg_hash, now as i64).unwrap(); model.votes.insert((t.to_base(),r.id),r.msg_hash); model.vote_times.insert((now,t.to_base(),r.id)); }
                _ => { db.cleanup_old_votes(age as i64, now as i64).unwrap(); model.cleanup_votes(now.wrapping_sub(age)); }
            }
            model.check(&db, &probes);
        }
    }
}

#[test]
fn platform_old_object_cleanup_erases_shared_message_and_session_indexes() {
    let mut db = db();
    let old = sig(P, 1, 2, 3);
    let mut newer = old.clone();
    newer.sig[0] ^= 1;
    db.write(&old, 10).unwrap();
    db.write(&newer, 100).unwrap();
    db.cleanup_old(60, 100).unwrap();
    assert!(!db.has_for_hash(&old.hash()).unwrap());
    assert!(db.has_for_hash(&newer.hash()).unwrap());
    assert_eq!(db.get_by_hash(&newer.hash()).unwrap(), Some(newer));
    assert!(!db.has_recovered_sig(P, &old.id, &old.msg_hash).unwrap());
    assert!(!db.has_for_session(&old.sign_hash()).unwrap());
}

#[test]
fn platform_read_gate_uses_configured_type_after_reopening() {
    let mut original = db();
    let r = sig(P, 1, 2, 3);
    original.write(&r, 10).unwrap();
    let changed = RecSigDb::new(original.into_inner(), T);
    assert!(!changed.has_recovered_sig(P, &r.id, &r.msg_hash).unwrap());
    assert_eq!(changed.get(P, &r.id, &r.msg_hash).unwrap(), None);
    assert_eq!(changed.get_by_hash(&r.hash()).unwrap(), Some(r));
}

#[test]
fn write_pins_platform_time_and_vote_value_bytes() {
    let mut db = db();
    let p = sig(P, 1, 2, 3);
    db.write(&p, 0x01020304).unwrap();
    db.write_vote(T, &[5; 32], &[6; 32], 0x01020304).unwrap();
    let r = sample();
    db.write(&r, 0x01020304).unwrap();
    let rows = &db.store().rows;
    assert_eq!(rows[&keys::recovered_platform(&p.hash())], encode_sig(&p));
    assert_eq!(rows[&keys::recovered_platform_message(P, &p.id, &p.msg_hash)], p.hash());
    assert_eq!(rows[&keys::recovered_platform_time(0x01020304, &p.hash())], [1]);
    assert_eq!(rows[&keys::recovered_time(0x01020304, T, &r.id)], [1]);
    assert_eq!(rows[&keys::vote(T, &[5; 32])], [6; 32]);
    assert_eq!(rows[&keys::vote_time(0x01020304, T, &[5; 32])], [1]);
}

#[test]
fn over_long_time_keys_erase_the_reencoded_key_and_stay() {
    let mut db = db();
    let r = sample();
    let p = sig(P, 1, 2, 3);
    db.write(&r, 10).unwrap();
    db.write(&p, 10).unwrap();
    db.write_vote(T, &r.id, &r.msg_hash, 10).unwrap();
    let canonical = [
        keys::recovered_platform_time(10, &p.hash()),
        keys::recovered_time(10, T, &r.id),
        keys::vote_time(10, T, &r.id),
    ];
    let long: Vec<Vec<u8>> = canonical.iter().map(|k| [k.clone(), vec![0xaa]].concat()).collect();
    for key in &long {
        db.kv.rows.insert(key.clone(), vec![1]);
    }
    db.cleanup_old(60, 100).unwrap();
    db.cleanup_old_votes(60, 100).unwrap();
    for (canonical, long) in canonical.iter().zip(&long) {
        assert!(!db.store().rows.contains_key(canonical));
        assert!(db.store().rows.contains_key(long));
    }
    assert!(!db.has_for_id(T, &r.id).unwrap());
    assert!(!db.has_for_hash(&p.hash()).unwrap());
    assert!(!db.has_voted(T, &r.id).unwrap());
}

#[test]
fn short_platform_and_vote_time_keys_stop_their_scans() {
    let mut db = db();
    let p = sig(P, 1, 2, 3);
    db.write(&p, 10).unwrap();
    db.write_vote(T, &[1; 32], &[2; 32], 10).unwrap();
    db.kv.rows.insert(b"\x04rs_u\0\0\0\x01".to_vec(), vec![1]);
    db.kv.rows.insert(b"\x05rs_vt\0\0\0\x01".to_vec(), vec![1]);
    db.cleanup_old(0, 100).unwrap();
    db.cleanup_old_votes(0, 100).unwrap();
    assert_eq!(db.get_by_hash(&p.hash()).unwrap(), Some(p));
    assert!(db.has_voted(T, &[1; 32]).unwrap());
}

#[test]
fn platform_gate_compares_numeric_type_values() {
    let shadow = LlmqType::Unknown(P.to_base());
    let mut db = RecSigDb::new(Mem::default(), shadow);
    let p = sig(P, 1, 2, 3);
    db.write(&p, 10).unwrap();
    assert!(db.store().rows.contains_key(&keys::recovered_platform(&p.hash())));
    assert!(!db.has_for_id(P, &p.id).unwrap());
    assert!(db.has_recovered_sig(P, &p.id, &p.msg_hash).unwrap());
    assert_eq!(db.get(P, &p.id, &p.msg_hash).unwrap(), Some(p));
}

fn id_of(n: u32) -> [u8; 32] {
    let mut id = [0; 32];
    id[..4].copy_from_slice(&n.to_be_bytes());
    id
}

fn cleanup_batches(db: &RecSigDb<Mem>, from: usize) -> &[RecSigBatch] {
    let writes = &db.store().writes[from..];
    assert!(writes.len() > 1, "expected an intermediate flush, got {} batch(es)", writes.len());
    assert!(writes[0].size_estimate() >= BATCH_FLUSH_BYTES);
    writes
}

#[test]
fn regular_cleanup_flushes_at_the_batch_threshold_and_erases_time_keys_last() {
    let mut db = db();
    let n = 100_000u32;
    for i in 0..n {
        let mut r = sample();
        r.id = id_of(i);
        db.kv.rows.insert(keys::recovered(T, &r.id), encode_sig(&r));
        db.kv
            .rows
            .insert(keys::recovered_message(T, &r.id, &r.msg_hash), 10u32.to_le_bytes().to_vec());
        db.kv.rows.insert(keys::recovered_time(10, T, &r.id), vec![1]);
    }
    let from = db.store().writes.len() + 1; // skip the Platform pass's batch
    db.cleanup_old(60, 100).unwrap();
    let batches = cleanup_batches(&db, from);
    let time_erases = |b: &RecSigBatch| {
        b.ops
            .iter()
            .filter(|op| matches!(op, RecSigOp::Delete(k) if k.starts_with(b"\x04rs_t")))
            .count()
    };
    let (last, earlier) = batches.split_last().unwrap();
    assert!(earlier.iter().all(|b| time_erases(b) == 0));
    assert_eq!(time_erases(last), n as usize);
    assert!(db.store().rows.is_empty());
}

#[test]
fn platform_cleanup_flushes_at_the_batch_threshold() {
    let mut db = db();
    let n = 100_000u32;
    for i in 0..n {
        let mut p = sig(P, 1, 2, 3);
        p.id = id_of(i);
        let hash = p.hash();
        db.kv.rows.insert(keys::recovered_platform(&hash), encode_sig(&p));
        db.kv.rows.insert(keys::recovered_platform_time(10, &hash), vec![1]);
    }
    let from = db.store().writes.len();
    db.cleanup_old(60, 100).unwrap();
    cleanup_batches(&db, from);
    assert!(db.store().rows.is_empty());
}
