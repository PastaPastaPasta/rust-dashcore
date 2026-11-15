use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use dash_consensus::primitives::CoreWrite;
use dash_params::{ChainParamsBuilder, ChainSetup, Network, Release};

use super::*;
use crate::dkg::bls::{BlsSchemeKind, BlsSkValue, secret_to_public, sign};
use crate::dkg::rng::SeededRng;
use crate::dkg::wire::QuorumHash;
use crate::sigenv::fakes::{FakeLookup, FakeQuorum};
use crate::signing::db::{RecSigBatch, RecSigOp, RecSigScan};
use crate::signing::intake::IntakeStatus;
use crate::signing::keys;

const T: LlmqType = LlmqType::LlmqTest;
const P: LlmqType = LlmqType::LlmqTestPlatform;
const BASIC: BlsCtx = BlsCtx::from_legacy(false);
const LEGACY: BlsCtx = BlsCtx::from_legacy(true);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Error {
    Read,
    Write,
    Lookup,
}

#[derive(Default)]
struct Mem {
    rows: BTreeMap<Vec<u8>, Vec<u8>>,
    fail_read: Cell<bool>,
    fail_read_at: Cell<Option<usize>>,
    fail_write: Cell<bool>,
    reads: Cell<usize>,
}

impl RecSigKv for Mem {
    type Error = Error;
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Error> {
        self.reads.set(self.reads.get() + 1);
        if self.fail_read.get() || self.fail_read_at.get() == Some(self.reads.get()) {
            return Err(Error::Read);
        }
        Ok(self.rows.get(key).cloned())
    }
    fn scan_from(&self, start: &[u8]) -> Result<RecSigScan<'_, Error>, Error> {
        if self.fail_read.get() {
            return Err(Error::Read);
        }
        Ok(Box::new(self.rows.range(start.to_vec()..).map(|(k, v)| Ok((k.clone(), v.clone())))))
    }
    fn write(&mut self, batch: RecSigBatch) -> Result<(), Error> {
        if self.fail_write.get() {
            return Err(Error::Write);
        }
        for op in batch.ops {
            match op {
                RecSigOp::Put(k, v) => {
                    self.rows.insert(k, v);
                }
                RecSigOp::Delete(k) => {
                    self.rows.remove(&k);
                }
            }
        }
        Ok(())
    }
}

#[derive(Default)]
struct Lookup {
    inner: FakeLookup,
    calls: RefCell<Vec<&'static str>>,
    fail_active: Cell<bool>,
    fail_get: Cell<bool>,
}

impl QuorumLookup for Lookup {
    type Quorum = FakeQuorum;
    type Error = Error;
    fn get_quorum(&self, t: LlmqType, h: &QuorumHash) -> Result<Option<Arc<FakeQuorum>>, Error> {
        self.calls.borrow_mut().push("get");
        if self.fail_get.get() {
            return Err(Error::Lookup);
        }
        Ok(self.inner.get_quorum(t, h).unwrap())
    }
    fn is_quorum_active(&self, t: LlmqType, h: &QuorumHash) -> Result<bool, Error> {
        self.calls.borrow_mut().push("active");
        if self.fail_active.get() {
            return Err(Error::Lookup);
        }
        Ok(self.inner.is_quorum_active(t, h).unwrap())
    }
    fn select_quorum_for_signing(
        &self,
        t: LlmqType,
        id: &[u8; 32],
        height: i32,
        offset: i32,
    ) -> Result<Option<Arc<FakeQuorum>>, Error> {
        Ok(self.inner.select_quorum_for_signing(t, id, height, offset).unwrap())
    }
}

fn params() -> ChainSetup {
    ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap()
}

fn key(t: LlmqType) -> BlsSkValue {
    let mut bytes = [0; 32];
    bytes[31] = if t == P {
        9
    } else {
        7
    };
    BlsSkValue::from_canonical(&bytes, BlsSchemeKind::Basic).unwrap()
}

fn lookup() -> Lookup {
    let mut l = Lookup::default();
    for t in [T, P] {
        for n in [3, 4] {
            let h = QuorumHash::from([n; 32]);
            let q = FakeQuorum {
                llmq_type: t,
                quorum_hash: h,
                params: *params().consensus.llmq(t).unwrap(),
                members: Vec::new(),
                valid_members: BTreeSet::new(),
                has_verification_vector: false,
                quorum_public_key: secret_to_public(&key(t)),
                pk_shares: BTreeMap::new(),
                sk_share: None,
            };
            l.inner.quorums.insert((t.to_base(), h), Arc::new(q));
            l.inner.active.insert((t.to_base(), h));
        }
    }
    l
}

fn manager() -> SigningManager<Mem> {
    SigningManager::new(Mem::default(), P, 60)
}

fn sig(t: LlmqType, id: u8, msg: u8, quorum: u8, bls: BlsCtx) -> Arc<RecoveredSig> {
    let mut r = RecoveredSig {
        llmq_type: t,
        quorum_hash: [quorum; 32].into(),
        id: [id; 32],
        msg_hash: [msg; 32],
        sig: [0; 96],
    };
    r.sig = sign(&key(t), &r.sign_hash(), bls.is_legacy()).to_bytes(bls.scheme());
    Arc::new(r)
}

fn wire(r: &RecoveredSig) -> Vec<u8> {
    let mut v = Vec::new();
    v.write(&[r.llmq_type.to_base()]);
    v.write(r.quorum_hash.as_bytes());
    v.write(&r.id);
    v.write(&r.msg_hash);
    v.write(&r.sig);
    v
}

fn enqueue(m: &mut SigningManager<Mem>, l: &Lookup, peer: PeerId, r: &RecoveredSig, bls: BlsCtx) {
    let outcome = m.on_qsigrec(peer, &wire(r), &params(), l, bls);
    assert_eq!(outcome.status, Ok(IntakeStatus::Queued));
    let completion = outcome.request_completion.unwrap();
    assert_eq!((completion.peer, completion.hash), (peer, r.hash()));
}

fn batch(m: &mut SigningManager<Mem>) -> PendingBatch {
    m.collect_pending(32, &mut SeededRng::new(42)).unwrap()
}

#[test]
fn storage_rules_platform_conflicts_and_listener_snapshots() {
    let mut m = manager();
    let a = sig(T, 1, 2, 3, BASIC);
    let b = sig(T, 1, 5, 3, BASIC);
    let same_message = sig(T, 1, 2, 4, BASIC);
    m.register_listener(7);
    m.register_listener(7);
    m.register_listener(9);
    m.write_vote(T, &a.id, &a.msg_hash, 100).unwrap();
    let event = m.process_recovered(a.clone(), true, 100).unwrap().unwrap();
    assert_eq!(
        event,
        RecoveredEvent {
            sig: a.clone(),
            from_self: true,
            proactive_relay: true,
            listeners: vec![7, 7, 9]
        }
    );
    m.unregister_listener(7);
    assert_eq!(m.listeners(), &[9]);
    assert_eq!(event.listeners, [7, 7, 9]);
    for r in [&a, &b, &same_message] {
        assert!(m.process_recovered(r.clone(), false, 101).unwrap().is_none());
    }
    assert!(!m.already_have(&b.hash()).unwrap());
    assert!(m.has_for_signing(T, &a.id, &b.msg_hash).unwrap());
    assert!(m.is_conflicting(T, &a.id, &b.msg_hash).unwrap());
    assert!(!m.is_conflicting(T, &a.id, &a.msg_hash).unwrap());
    assert!(!m.is_conflicting(T, &[99; 32], &a.msg_hash).unwrap());
    assert_eq!(m.vote_for(T, &a.id).unwrap(), Some(a.msg_hash));
    assert!(RecSigReads::has_for_session(&m, &a.sign_hash()).unwrap());
    assert!(RecSigReads::has_for_signing(&m, T, &a.id, &b.msg_hash).unwrap());

    let a = sig(P, 1, 2, 3, BASIC);
    let b = sig(P, 1, 5, 3, BASIC);
    assert!(!m.has_for_signing(P, &a.id, &a.msg_hash).unwrap());
    m.process_recovered(a.clone(), false, 100).unwrap().unwrap();
    assert!(!m.has_for_signing(P, &b.id, &b.msg_hash).unwrap());
    m.process_recovered(b.clone(), false, 100).unwrap().unwrap();
    assert!(m.has_for_signing(P, &b.id, &b.msg_hash).unwrap());
    assert!(!m.is_conflicting(P, &b.id, &[99; 32]).unwrap());
    assert_eq!(m.db().get_by_hash(&a.hash()).unwrap(), Some((*a).clone()));
    assert_eq!(m.db().get_by_hash(&b.hash()).unwrap(), Some((*b).clone()));
    assert!(m.process_recovered(sig(P, 1, 2, 4, BASIC), true, 100).unwrap().is_none());
}

#[test]
fn platform_by_id_rows_are_not_conflicts() {
    let mut m = manager();
    let a = sig(P, 9, 2, 3, BASIC);
    let mut batch = RecSigBatch::default();
    batch.put(keys::recovered(P, &a.id), wire(&a));
    batch.put(keys::recovered_message(P, &a.id, &a.msg_hash), 100u32.to_le_bytes().to_vec());
    m.db.store_mut().write(batch).unwrap();
    assert!(!m.is_conflicting(P, &a.id, &[99; 32]).unwrap());
}

#[test]
fn proactive_relay_respects_core_type_exclusions() {
    for (t, allowed) in [
        (T, true),
        (P, true),
        (LlmqType::Llmq100_67, false),
        (LlmqType::Llmq400_60, false),
        (LlmqType::Llmq400_85, false),
    ] {
        for from_self in [false, true] {
            let mut m = manager();
            let r = sig(t, 1, 2, 3, BASIC);
            let event = m.process_recovered(r, from_self, 100).unwrap().unwrap();
            assert_eq!(event.from_self, from_self);
            assert_eq!(event.proactive_relay, from_self && allowed);
        }
    }
}

#[test]
fn inventory_getdata_reconstruction_and_truncation() {
    let mut m = manager();
    let mut l = lookup();
    let a = sig(T, 1, 2, 3, BASIC);
    enqueue(&mut m, &l, 1, &a, BASIC);
    assert!(!m.already_have(&a.hash()).unwrap());
    m.push_reconstructed(a.clone());
    m.push_reconstructed(a.clone());
    assert!(m.already_have(&a.hash()).unwrap());
    assert_eq!(m.recsig_for_getdata(&a.hash(), &l).unwrap(), None);
    m.remove_node(1);
    let reconstructed = m.take_reconstructed();
    assert_eq!(reconstructed, vec![a.clone()]);
    assert!(m.take_reconstructed().is_empty());
    assert!(!m.already_have(&a.hash()).unwrap());
    m.process_recovered(a.clone(), true, 100).unwrap().unwrap();
    assert_eq!(m.recsig_for_getdata(&a.hash(), &l).unwrap(), Some((*a).clone()));
    l.inner.active.clear();
    assert_eq!(m.recsig_for_getdata(&a.hash(), &l).unwrap(), None);
    assert!(m.already_have(&a.hash()).unwrap());
    m.truncate(T, &a.id).unwrap();
    assert!(m.already_have(&a.hash()).unwrap());
    assert!(RecSigReads::has_for_session(&m, &a.sign_hash()).unwrap());
    assert!(!m.has_for_signing(T, &a.id, &a.msg_hash).unwrap());
    assert_eq!(m.recsig_for_getdata(&a.hash(), &l).unwrap(), None);
    assert!(!batch(&mut m).more_work);
}

#[test]
fn reconstructed_work_keeps_more_work_and_is_erased_after_processing() {
    let mut m = manager();
    let l = lookup();
    let a = sig(T, 1, 2, 3, BASIC);
    m.push_reconstructed(a.clone());
    m.process_recovered(a, true, 100).unwrap().unwrap();
    assert!(m.take_reconstructed().is_empty());

    let a = sig(T, 2, 2, 3, BASIC);
    let b = sig(T, 3, 2, 3, BASIC);
    enqueue(&mut m, &l, 1, &a, BASIC);
    enqueue(&mut m, &l, 1, &b, BASIC);
    m.process_recovered(a.clone(), true, 100).unwrap();
    m.push_reconstructed(sig(T, 4, 2, 3, BASIC));
    let first = m.collect_pending(1, &mut SeededRng::new(1)).unwrap();
    assert!(first.more_work);

    let mut control = manager();
    enqueue(&mut control, &l, 1, &b, BASIC);
    let control_batch = control.collect_pending(1, &mut SeededRng::new(1)).unwrap();
    assert!(!control_batch.more_work);
}

#[test]
fn intake_preserves_request_completion_and_gate_order() {
    let mut m = manager();
    let mut l = lookup();
    let a = sig(T, 1, 2, 3, BASIC);
    let mut bytes = wire(&a);
    let short = m.on_qsigrec(8, &bytes[..10], &params(), &l, BASIC);
    assert_eq!(short.status, Ok(IntakeStatus::Unreadable));
    assert!(short.request_completion.is_none());
    bytes[0] = 255;
    let unknown = m.on_qsigrec(8, &bytes, &params(), &l, BASIC);
    assert_eq!(unknown.status, Ok(IntakeStatus::BanUnknownType));
    assert_eq!(unknown.request_completion.unwrap().peer, 8);
    assert!(l.calls.borrow().is_empty());
    assert_eq!(m.db().store().reads.get(), 0);

    m.process_recovered(a.clone(), false, 100).unwrap();
    l.inner.active.clear();
    m.db().store().fail_read.set(true);
    let inactive = m.on_qsigrec(8, &wire(&a), &params(), &l, BASIC);
    assert_eq!(inactive.status, Ok(IntakeStatus::InactiveQuorum));
    assert_eq!(*l.calls.borrow(), ["active"]);
    m.db().store().fail_read.set(false);
    l.inner.active.insert((T.to_base(), a.quorum_hash));
    l.calls.borrow_mut().clear();
    assert_eq!(
        m.on_qsigrec(8, &wire(&a), &params(), &l, BASIC).status,
        Ok(IntakeStatus::AlreadyKnown)
    );
    assert_eq!(*l.calls.borrow(), ["active"]);

    let b = sig(T, 2, 2, 3, BASIC);
    m.push_reconstructed(b.clone());
    l.inner.quorums.clear();
    assert_eq!(
        m.on_qsigrec(8, &wire(&b), &params(), &l, BASIC).status,
        Ok(IntakeStatus::QuorumUnavailable)
    );
    l = lookup();
    assert_eq!(
        m.on_qsigrec(8, &wire(&b), &params(), &l, BASIC).status,
        Ok(IntakeStatus::PendingReconstruction)
    );
    assert_eq!(*l.calls.borrow(), ["active", "get"]);

    let mut bytes = wire(&sig(T, 3, 2, 3, BASIC));
    bytes.extend_from_slice(&[42; 3]);
    assert_eq!(m.on_qsigrec(8, &bytes, &params(), &l, BASIC).status, Ok(IntakeStatus::Queued));
}

#[test]
fn queue_caps_and_remove_node_release_capacity() {
    let mut m = manager();
    let l = lookup();
    let a = sig(T, 1, 2, 3, BASIC);
    let bytes = wire(&a);
    let p = params();
    for peer in 0..10 {
        for _ in 0..MAX_PENDING_PER_PEER {
            assert_eq!(m.on_qsigrec(peer, &bytes, &p, &l, BASIC).status, Ok(IntakeStatus::Queued));
        }
        assert_eq!(m.on_qsigrec(peer, &bytes, &p, &l, BASIC).status, Ok(IntakeStatus::QueueFull));
    }
    assert_eq!(m.on_qsigrec(11, &bytes, &p, &l, BASIC).status, Ok(IntakeStatus::QueueFull));
    m.push_reconstructed(sig(T, 9, 2, 3, BASIC));
    m.remove_node(0);
    m.remove_node(0);
    for _ in 0..MAX_PENDING_PER_PEER {
        assert_eq!(m.on_qsigrec(11, &bytes, &p, &l, BASIC).status, Ok(IntakeStatus::Queued));
    }
    assert_eq!(m.on_qsigrec(12, &bytes, &p, &l, BASIC).status, Ok(IntakeStatus::QueueFull));
    assert_eq!(m.take_reconstructed().len(), 1);
    for peer in 1..=11 {
        m.remove_node(peer);
    }
    assert!(batch(&mut m).by_source.is_empty());
    enqueue(&mut m, &l, 12, &a, BASIC);
    assert_eq!(batch(&mut m).by_source[&12].len(), 1);
}

#[test]
fn unique_pair_bound_repeats_and_known_drops() {
    let mut m = manager();
    let l = lookup();
    let repeated = sig(T, 1, 2, 3, BASIC);
    let known = sig(T, 200, 2, 3, BASIC);
    for peer in 0..4 {
        enqueue(&mut m, &l, peer, &known, BASIC);
        let count = if peer == 0 {
            40
        } else {
            12
        };
        for n in 0..count {
            let r = if peer == 0 {
                repeated.clone()
            } else {
                sig(T, (peer * 20 + n) as u8, 2, 3, BASIC)
            };
            enqueue(&mut m, &l, peer, &r, BASIC);
        }
    }
    m.process_recovered(known.clone(), true, 100).unwrap();
    let first = batch(&mut m);
    let unique: BTreeSet<_> = first
        .by_source
        .iter()
        .flat_map(|(p, v)| v.iter().map(|r| (*p, r.sig.sign_hash())))
        .collect();
    let count: usize = first.by_source.values().map(Vec::len).sum();
    assert_eq!(unique.len(), 32);
    assert!(count > 32);
    assert_eq!(first.by_source.values().map(Vec::len).collect::<Vec<_>>(), [10, 10, 11, 10]);
    assert!(first.more_work);
    assert!(first.by_source.values().flatten().all(|r| r.sig.hash() != known.hash()));
    let second = batch(&mut m);
    assert_eq!(count + second.by_source.values().map(Vec::len).sum::<usize>(), 40 + 3 * 12);
    assert!(!second.more_work);
    assert!(batch(&mut m).by_source.is_empty());
}

#[test]
fn round_robin_collection_includes_a_single_item_peer() {
    let mut m = manager();
    let l = lookup();
    for id in 0..8 {
        enqueue(&mut m, &l, 1, &sig(T, id, 2, 3, BASIC), BASIC);
    }
    enqueue(&mut m, &l, 2, &sig(T, 20, 2, 3, BASIC), BASIC);
    let batch = m.collect_pending(2, &mut SeededRng::new(1)).unwrap();
    assert_eq!(batch.by_source[&1].len(), 1);
    assert_eq!(batch.by_source[&2].len(), 1);
}

#[test]
fn collection_shuffle_visits_different_peers_first() {
    let l = lookup();
    let mut first = BTreeSet::new();
    for seed in 0..32 {
        let mut m = manager();
        for peer in 0..4 {
            enqueue(&mut m, &l, peer, &sig(T, peer as u8, 2, 3, BASIC), BASIC);
        }
        let batch = m.collect_pending(1, &mut SeededRng::new(seed)).unwrap();
        first.extend(batch.by_source.keys().copied());
    }
    assert_eq!(first.len(), 4);
}

#[test]
fn pair_bound_counts_sources_and_zero_leaves_queue() {
    let mut m = manager();
    let l = lookup();
    let a = sig(T, 1, 2, 3, BASIC);
    for peer in 0..3 {
        enqueue(&mut m, &l, peer, &a, BASIC);
    }
    let zero = m.collect_pending(0, &mut SeededRng::new(1)).unwrap();
    assert!(zero.by_source.is_empty());
    assert!(!zero.more_work);
    let first = m.collect_pending(2, &mut SeededRng::new(1)).unwrap();
    assert_eq!(first.by_source.len(), 2);
    assert!(first.more_work);
    assert_eq!(batch(&mut m).by_source.len(), 1);
}

#[test]
fn seeded_collection_is_reproducible() {
    let l = lookup();
    let mut a = manager();
    let mut b = manager();
    for peer in 0..8 {
        let r = sig(T, peer as u8, 2, 3, BASIC);
        enqueue(&mut a, &l, peer, &r, BASIC);
        enqueue(&mut b, &l, peer, &r, BASIC);
    }
    let x = a.collect_pending(3, &mut SeededRng::new(17)).unwrap();
    let y = b.collect_pending(3, &mut SeededRng::new(17)).unwrap();
    assert_eq!(x.by_source.keys().collect::<Vec<_>>(), y.by_source.keys().collect::<Vec<_>>());
    assert!(x.more_work && y.more_work);
}

#[test]
fn batch_verifies_per_source_and_deduplicates_good_signatures() {
    let mut m = manager();
    let l = lookup();
    let a = sig(T, 1, 2, 3, BASIC);
    let b = sig(T, 2, 2, 3, BASIC);
    let c = sig(P, 1, 2, 3, BASIC);
    let mut wrong = (*b).clone();
    wrong.msg_hash = [99; 32];
    let mut malformed = (*b).clone();
    malformed.sig = [0; 96];
    let inputs: [(PeerId, &RecoveredSig); 7] =
        [(0, &a), (0, &wrong), (1, &a), (1, &a), (2, &malformed), (2, &b), (-1, &c)];
    for (peer, r) in inputs {
        enqueue(&mut m, &l, peer, r, BASIC);
    }
    m.register_listener(12);
    let pending = batch(&mut m);
    let events = m.process_batch(pending, &l, BASIC, 100);
    let bans: BTreeSet<_> = events
        .iter()
        .filter_map(|e| match e {
            SigningEvent::Ban {
                peer,
                mark_shares_banned: false,
            } => Some(*peer),
            _ => None,
        })
        .collect();
    assert_eq!(bans, [0, 2].into());
    let recovered: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            SigningEvent::Recovered(e) => Some(e),
            _ => None,
        })
        .collect();
    assert_eq!(recovered.len(), 2);
    assert_eq!(recovered[0].sig, c);
    assert!(recovered[0].from_self);
    assert_eq!(recovered[1].sig, a);
    assert!(!recovered[1].from_self);
    assert_eq!(recovered[1].listeners, [12]);
    assert!(!m.already_have(&b.hash()).unwrap());
}

#[test]
fn bad_source_purges_uncollected_queue() {
    let mut m = manager();
    let l = lookup();
    let mut bad = (*sig(T, 1, 2, 3, BASIC)).clone();
    bad.sig = [0; 96];
    enqueue(&mut m, &l, 4, &bad, BASIC);
    enqueue(&mut m, &l, 4, &sig(T, 2, 2, 3, BASIC), BASIC);
    let pending = m.collect_pending(1, &mut SeededRng::new(1)).unwrap();
    assert!(pending.more_work);
    assert!(matches!(
        m.process_batch(pending, &l, BASIC, 100).as_slice(),
        [SigningEvent::Ban {
            peer: 4,
            mark_shares_banned: false
        }]
    ));
    assert!(batch(&mut m).by_source.is_empty());
}

#[test]
fn unknown_type_ban_purges_peer_queue_and_invalid_self_is_not_banned() {
    let mut m = manager();
    let l = lookup();
    let a = sig(T, 1, 2, 3, BASIC);
    enqueue(&mut m, &l, 4, &a, BASIC);
    let mut unknown = wire(&a);
    unknown[0] = 255;
    let outcome = m.on_qsigrec(4, &unknown, &params(), &l, BASIC);
    assert_eq!(outcome.status, Ok(IntakeStatus::BanUnknownType));
    assert!(outcome.request_completion.is_some());
    assert!(batch(&mut m).by_source.is_empty());

    let mut bad = (*a).clone();
    bad.sig = [0; 96];
    enqueue(&mut m, &l, -1, &bad, BASIC);
    enqueue(&mut m, &l, -1, &sig(T, 2, 2, 3, BASIC), BASIC);
    let pending = m.collect_pending(1, &mut SeededRng::new(1)).unwrap();
    assert!(m.process_batch(pending, &l, BASIC, 100).is_empty());
    assert_eq!(batch(&mut m).by_source[&-1].len(), 1);
}

#[test]
fn batch_rechecks_missing_and_inactive_quorums_without_bans() {
    for missing in [true, false] {
        let mut m = manager();
        let mut l = lookup();
        let a = sig(T, 1, 2, 3, BASIC);
        let b = sig(T, 2, 2, 4, BASIC);
        enqueue(&mut m, &l, 1, &a, BASIC);
        enqueue(&mut m, &l, 1, &b, BASIC);
        let pending = batch(&mut m);
        if missing {
            l.inner.quorums.remove(&(T.to_base(), a.quorum_hash));
        } else {
            l.inner.active.remove(&(T.to_base(), a.quorum_hash));
        }
        let events = m.process_batch(pending, &l, BASIC, 100);
        assert!(
            matches!(&events[0], SigningEvent::QuorumUnavailable { peer: 1, inactive, .. } if *inactive != missing)
        );
        assert!(matches!(&events[1], SigningEvent::Recovered(e) if e.sig == b));
        assert_eq!(events.len(), 2);
        assert!(!m.already_have(&a.hash()).unwrap());
        assert!(batch(&mut m).by_source.is_empty());
    }
}

#[test]
fn scheme_is_captured_at_intake_and_sampled_at_verification() {
    for read in [BASIC, LEGACY] {
        for check in [BASIC, LEGACY] {
            let mut m = manager();
            let l = lookup();
            let a = sig(T, 1, 2, 3, read);
            enqueue(&mut m, &l, 1, &a, read);
            let pending = batch(&mut m);
            let events = m.process_batch(pending, &l, check, 100);
            if read == check {
                assert!(matches!(events.as_slice(), [SigningEvent::Recovered(_)]));
                assert!(m.already_have(&a.hash()).unwrap());
            } else {
                assert!(matches!(
                    events.as_slice(),
                    [SigningEvent::Ban {
                        peer: 1,
                        ..
                    }]
                ));
                assert!(!m.already_have(&a.hash()).unwrap());
            }
        }
    }
    for (read, check) in [(LEGACY, BASIC), (BASIC, LEGACY)] {
        let mut m = manager();
        let l = lookup();
        let mut a = (*sig(T, 1, 2, 3, check)).clone();
        a.sig = sign(&key(T), &a.sign_hash(), check.is_legacy()).to_bytes(read.scheme());
        enqueue(&mut m, &l, 1, &a, read);
        let pending = batch(&mut m);
        assert!(matches!(
            m.process_batch(pending, &l, check, 100).as_slice(),
            [SigningEvent::Recovered(_)]
        ));
        assert!(m.already_have(&a.hash()).unwrap());
    }
}

#[test]
fn cleanup_expiry_is_strict_and_throttled_including_votes() {
    let mut m = manager();
    let a = sig(T, 1, 2, 3, BASIC);
    let p = sig(P, 1, 2, 3, BASIC);
    for r in [&a, &p] {
        m.process_recovered(r.clone(), true, 100).unwrap();
    }
    m.write_vote(T, &[9; 32], &[8; 32], 100).unwrap();
    assert!(m.cleanup(160000).unwrap());
    assert!(m.already_have(&a.hash()).unwrap());
    assert!(m.already_have(&p.hash()).unwrap());
    assert_eq!(m.vote_for(T, &[9; 32]).unwrap(), Some([8; 32]));
    assert!(!m.cleanup(161000).unwrap());
    assert!(!m.cleanup(164999).unwrap());
    assert!(m.cleanup(165000).unwrap());
    assert!(!m.already_have(&a.hash()).unwrap());
    assert!(!m.already_have(&p.hash()).unwrap());
    assert_eq!(m.vote_for(T, &[9; 32]).unwrap(), None);
    assert!(!m.cleanup(100000).unwrap());
}

#[test]
fn failures_are_reported_without_bans_or_notifications() {
    let mut m = manager();
    let l = lookup();
    let a = sig(T, 1, 2, 3, BASIC);
    l.fail_active.set(true);
    let intake = m.on_qsigrec(1, &wire(&a), &params(), &l, BASIC);
    assert_eq!(intake.status, Err(SigningError::Lookup(Error::Lookup)));
    assert_eq!(intake.request_completion.unwrap().hash, a.hash());
    l.fail_active.set(false);
    m.db().store().fail_read.set(true);
    assert_eq!(
        m.on_qsigrec(1, &wire(&a), &params(), &l, BASIC).status,
        Err(SigningError::Storage(Error::Read))
    );
    m.db().store().fail_read.set(false);
    enqueue(&mut m, &l, 1, &a, BASIC);
    let pending = batch(&mut m);
    l.fail_get.set(true);
    assert!(matches!(
        m.process_batch(pending, &l, BASIC, 100).as_slice(),
        [SigningEvent::LookupError {
            error: Error::Lookup,
            ..
        }]
    ));
    l.fail_get.set(false);
    enqueue(&mut m, &l, 1, &a, BASIC);
    enqueue(&mut m, &l, 1, &a, BASIC);
    let pending = batch(&mut m);
    m.db().store().fail_write.set(true);
    assert!(matches!(
        m.process_batch(pending, &l, BASIC, 100).as_slice(),
        [SigningEvent::StorageError {
            hash,
            peer: 1,
            error: Error::Write,
        }] if *hash == a.hash()
    ));
    assert!(!m.already_have(&a.hash()).unwrap());
    m.db().store().fail_write.set(false);
    let event = m.process_recovered(a.clone(), true, 100).unwrap();
    assert!(event.is_some());
    l.fail_active.set(true);
    assert!(matches!(
        m.recsig_for_getdata(&a.hash(), &l),
        Err(SigningError::Lookup(Error::Lookup))
    ));
    m.db().store().fail_read.set(true);
    assert!(matches!(m.recsig_for_getdata(&a.hash(), &l), Err(SigningError::Storage(Error::Read))));
}

#[test]
fn collection_read_error_restores_collected_work() {
    let mut m = manager();
    let l = lookup();
    for id in 0..3 {
        enqueue(&mut m, &l, 1, &sig(T, id, 2, 3, BASIC), BASIC);
    }
    let store = m.db().store();
    store.fail_read_at.set(Some(store.reads.get() + 2));
    assert!(matches!(m.collect_pending(32, &mut SeededRng::new(1)), Err(Error::Read)));
    m.db().store().fail_read_at.set(None);
    let pending = batch(&mut m);
    assert_eq!(pending.by_source[&1].iter().map(|r| r.sig.id[0]).collect::<Vec<_>>(), [0, 1, 2]);
    assert!(!pending.more_work);
}
