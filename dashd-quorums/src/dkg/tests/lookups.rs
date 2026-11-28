//! Item 5: the `getdata` lookups (`l/dkgsessionmgr.cpp:57-123`, `a/dkgsessionhandler.cpp:257-279`,
//! `l/dkgsession.cpp:660-694`, v24.0.0) over messages a reference round recorded.

use std::collections::BTreeSet;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};

use dash_llmq::dkg::conn::ConnSnapshot;
use dash_llmq::dkg::reference::{Network, seeded_rng};
use dash_llmq::dkg::rng::SeededRng;
use dash_llmq::dkg::session::{DkgMsgKind, ServedSink};
use dash_llmq::dkg::wire::{Complaint, Contribution, Justification, PrematureCommitment};
use dash_params::{LlmqParams, LlmqType};

use super::fakes::{Trace, params, seams, sessions, tip};
use crate::dkg::{DkgSessions, QuorumPhase, Served, SessionKey, phase_serves};

/// One recorded message of each kind, and the served side of the session that recorded them.
struct Round {
    contributions: Vec<Contribution>,
    complaint: Complaint,
    justification: Justification,
    pcommit: PrematureCommitment,
    served: Arc<Served>,
}

/// Forwards to a [`Served`] and keeps every hash the session told it.
struct Tee {
    served: Arc<Served>,
    seen: Mutex<BTreeSet<[u8; 32]>>,
}

impl Tee {
    fn see(&self, hash: &[u8; 32]) {
        self.seen.lock().expect("seen").insert(*hash);
    }
}

impl ServedSink for Tee {
    fn contribution(&self, hash: &[u8; 32], qc: &Arc<Contribution>) {
        self.see(hash);
        ServedSink::contribution(self.served.as_ref(), hash, qc);
    }
    fn complaint(&self, hash: &[u8; 32], qc: &Arc<Complaint>) {
        self.see(hash);
        ServedSink::complaint(self.served.as_ref(), hash, qc);
    }
    fn justification(&self, hash: &[u8; 32], qj: &Arc<Justification>) {
        self.see(hash);
        ServedSink::justification(self.served.as_ref(), hash, qj);
    }
    fn premature_commitment(&self, hash: &[u8; 32], qc: &Arc<PrematureCommitment>) {
        self.see(hash);
        ServedSink::premature_commitment(self.served.as_ref(), hash, qc);
    }
    fn valid_commitment(&self, hash: &[u8; 32]) {
        self.see(hash);
        ServedSink::valid_commitment(self.served.as_ref(), hash);
    }
}

/// A `llmq_test` round of three members in which member 0 lies to member 1, so that member 1
/// complains and member 0 justifies; member 2's session records into a [`Served`] through a
/// [`Tee`]. The network is returned as the round left it.
fn play() -> (Network, Round, Arc<Tee>) {
    let params = params(LlmqType::LlmqTest);
    let weights = || Box::new(SeededRng::new(0x77)) as _;
    let mut net = Network::new(LlmqType::LlmqTest, &params, 3, 0x1001, weights, seeded_rng);
    let served = Arc::new(Served::for_session(&net.nodes[2].session));
    let tee = Arc::new(Tee {
        served: served.clone(),
        seen: Mutex::new(BTreeSet::new()),
    });
    net.nodes[2].session = net.nodes[2].session.clone().with_served(tee.clone());
    let mut contributions = vec![net.contribute_modified(0, &BTreeSet::from([1]))];
    contributions.extend((1..3).map(|i| net.contribute(i).expect("a contribution")));
    net.drain(|_, _| 8);
    net.verify_pending();
    net.enter(DkgMsgKind::Complaint);
    let complaint = net.complain(1, &ConnSnapshot::default()).expect("a complaint");
    assert!(net.complain(0, &ConnSnapshot::default()).is_none());
    assert!(net.complain(2, &ConnSnapshot::default()).is_none());
    net.drain(|_, _| 8);
    net.enter(DkgMsgKind::Justification);
    let justification = net.justify(0, true).expect("a justification");
    assert!(net.justify(1, true).is_none() && net.justify(2, true).is_none());
    net.drain(|_, _| 8);
    net.enter(DkgMsgKind::PrematureCommitment);
    let bls = net.bls;
    let pcommit =
        net.nodes[2].step(bls, |s, env| s.verify_and_commit(env)).expect("a premature commitment");
    net.send(2, &pcommit.payload(bls.scheme()));
    net.drain(|_, _| 8);
    let round = Round {
        contributions,
        complaint,
        justification,
        pcommit,
        served,
    };
    (net, round, tee)
}

/// [`play`]'s round, played once.
fn round() -> &'static Round {
    static ROUND: OnceLock<Round> = OnceLock::new();
    ROUND.get_or_init(|| play().1)
}

const TEST: SessionKey = SessionKey {
    llmq_type: LlmqType::LlmqTest,
    quorum_index: 0,
};

/// Which kinds `sessions` serves: contribution, complaint, justification, premature commitment.
fn served(sessions: &DkgSessions, r: &Round) -> [bool; 4] {
    [
        sessions.get_contribution(r.contributions[0].session_hash()).is_some(),
        sessions.get_complaint(r.complaint.session_hash()).is_some(),
        sessions.get_justification(r.justification.session_hash()).is_some(),
        sessions.get_premature_commitment(r.pcommit.session_hash()).is_some(),
    ]
}

/// A tip of `llmq_test`'s interval at 48 in each phase, Initialized to Idle.
const TIPS: [(QuorumPhase, i32); 7] = [
    (QuorumPhase::Initialized, 48),
    (QuorumPhase::Contribute, 50),
    (QuorumPhase::Complain, 52),
    (QuorumPhase::Justify, 54),
    (QuorumPhase::Commit, 56),
    (QuorumPhase::Finalize, 58),
    (QuorumPhase::Idle, 60),
];

#[test]
fn each_kind_is_served_only_in_its_two_phases() {
    let r = round();
    let trace = Arc::new(Trace::default());
    let (sessions, sporks) = sessions(&trace);
    sessions.handlers()[0].set_session(r.served.clone());
    let want = [
        [true, false, false, false],
        [true, true, false, false],
        [false, true, true, false],
        [false, false, true, true],
        [false, false, false, true],
        [false, false, false, false],
        [false, false, false, false],
    ];
    for ((phase, h), want) in TIPS.into_iter().zip(want) {
        sessions.updated_block_tip(&tip(h), false);
        assert_eq!(sessions.phase(TEST), phase);
        assert_eq!(served(&sessions, r), want, "{phase:?}");
        let kinds = [
            DkgMsgKind::Contribution,
            DkgMsgKind::Complaint,
            DkgMsgKind::Justification,
            DkgMsgKind::PrematureCommitment,
        ];
        assert_eq!(kinds.map(|k| phase_serves(k, phase)), want, "{phase:?}");

        sporks.spork17.store(false, Ordering::SeqCst);
        assert_eq!(served(&sessions, r), [false; 4], "spork 17 off, {phase:?}");
        sporks.spork17.store(true, Ordering::SeqCst);

        let held = sessions.handlers()[0].take_session().expect("the session");
        assert_eq!(served(&sessions, r), [false; 4], "null slot, {phase:?}");
        sessions.handlers()[0].set_session(held);
    }
}

#[test]
fn every_recorded_message_is_served_as_recorded() {
    let r = round();
    for qc in &r.contributions {
        let got = r.served.contribution(qc.session_hash()).expect("recorded");
        assert_eq!(got.pro_tx_hash(), qc.pro_tx_hash());
    }
    assert!(r.served.complaint(r.complaint.session_hash()).is_some());
    assert!(r.served.justification(r.justification.session_hash()).is_some());
    let got = r.served.premature_commitment(r.pcommit.session_hash()).expect("valid");
    assert_eq!(got.pro_tx_hash(), r.pcommit.pro_tx_hash());
    assert_eq!(r.served.my_index(), Some(2));
    assert!(r.served.contribution(&[0; 32]).is_none());
}

#[test]
fn a_premature_commitment_is_served_only_once_valid() {
    let r = round();
    let trace = Arc::new(Trace::default());
    let (sessions, _) = sessions(&trace);
    let served = Arc::new(Served::new(Some(0)));
    sessions.handlers()[0].set_session(served.clone());
    sessions.updated_block_tip(&tip(54), false);
    let hash = r.pcommit.session_hash();
    ServedSink::premature_commitment(served.as_ref(), hash, &Arc::new(r.pcommit.clone()));
    assert!(sessions.get_premature_commitment(hash).is_none());
    ServedSink::valid_commitment(served.as_ref(), hash);
    let got = sessions.get_premature_commitment(hash).expect("valid");
    assert_eq!(got.pro_tx_hash(), r.pcommit.pro_tx_hash());
}

#[test]
fn the_first_handler_in_key_order_that_serves_answers() {
    let r = round();
    let trace = Arc::new(Trace::default());
    let (seams, chain, _) = seams(&trace);
    chain.rotation.store(true, Ordering::SeqCst);
    let rotated = params(LlmqType::LlmqTestDip0024);
    let sessions = DkgSessions::new(
        [(LlmqType::LlmqTestDip0024, rotated), (LlmqType::LlmqTest, params(LlmqType::LlmqTest))],
        seams,
    );
    let key = |llmq_type, quorum_index| SessionKey {
        llmq_type,
        quorum_index,
    };
    assert_eq!(
        sessions.keys(),
        [
            key(LlmqType::LlmqTest, 0),
            key(LlmqType::LlmqTestDip0024, 0),
            key(LlmqType::LlmqTestDip0024, 1),
        ]
    );
    // The same hash in each rotated handler, a different contribution behind it.
    let hash = [0x5a; 32];
    let handlers = sessions.handlers();
    for (handler, qc) in handlers[1..].iter().zip(&r.contributions[1..]) {
        let s = Arc::new(Served::new(Some(0)));
        ServedSink::contribution(s.as_ref(), &hash, &Arc::new(qc.clone()));
        handler.set_session(s);
    }
    let first = r.contributions[1].pro_tx_hash();
    let second = r.contributions[2].pro_tx_hash();
    let mut height = 47;
    let mut answer = |to| {
        while height < to {
            height += 1;
            sessions.updated_block_tip(&tip(height), false);
        }
        let phases = [0, 1].map(|qi| sessions.phase(key(LlmqType::LlmqTestDip0024, qi)));
        (phases, sessions.get_contribution(&hash).map(|qc| *qc.pro_tx_hash()))
    };
    use QuorumPhase::{Complain, Contribute, Initialized};
    // 50: index 0 in Contribute, index 1 in Initialized; both serve, index 0 first.
    assert_eq!(answer(50), ([Contribute, Initialized], Some(*first)));
    // 52: index 0 in Complain is out of the window; index 1, in Contribute, answers.
    assert_eq!(answer(52), ([Complain, Contribute], Some(*second)));
    // 54: both out.
    assert_eq!(answer(54).1, None);
    assert_eq!(sessions.phase(key(LlmqType::LlmqTest, 9)), QuorumPhase::Idle, "an unknown key");
}

#[test]
fn an_observer_has_the_keys_stays_idle_and_serves_nothing() {
    let r = round();
    let trace = Arc::new(Trace::default());
    let (seams, chain, _) = seams(&trace);
    chain.rotation.store(true, Ordering::SeqCst);
    let llmqs = [
        (LlmqType::LlmqTest, params(LlmqType::LlmqTest)),
        (LlmqType::LlmqTestDip0024, params(LlmqType::LlmqTestDip0024)),
    ];
    let active = DkgSessions::new(llmqs, seams.clone());
    let observer = DkgSessions::observer(llmqs, seams);
    assert!(!observer.is_active());
    assert_eq!(observer.keys(), active.keys());
    assert!(observer.handlers().is_empty());
    trace.take();
    for h in [48, 50, 52, 54] {
        observer.updated_block_tip(&tip(h), false);
        for key in observer.keys() {
            assert_eq!(observer.phase(key), QuorumPhase::Idle);
        }
        assert_eq!(served(&observer, r), [false; 4]);
    }
    let calls = trace.take();
    assert_eq!(calls.iter().filter(|c| *c == "cleanup").count(), 4, "{calls:?}");
    assert!(!calls.contains(&"snapshot".to_string()), "{calls:?}");
}

#[test]
fn a_type_given_twice_keeps_its_first_parameters() {
    let trace = Arc::new(Trace::default());
    let (seams, _, _) = seams(&trace);
    let rotated = params(LlmqType::LlmqTestDip0024);
    let flat = LlmqParams {
        use_rotation: false,
        ..rotated
    };
    let sessions = DkgSessions::new(
        [(LlmqType::LlmqTestDip0024, rotated), (LlmqType::LlmqTestDip0024, flat)],
        seams,
    );
    assert_eq!(sessions.keys().len(), 2);
    assert!(sessions.handlers().iter().all(|h| h.params().use_rotation));
}

/// `served` is the very message `session` answers with, or both have none.
fn same<T>(served: Option<Arc<T>>, session: Option<&T>, what: &str, hash: &[u8; 32]) {
    match (served, session) {
        (None, None) => {}
        (Some(a), Some(b)) => assert!(std::ptr::eq(a.as_ref(), b), "{what} {hash:02x?}"),
        (a, b) => panic!("{what} {hash:02x?}: served {}, session {}", a.is_some(), b.is_some()),
    }
}

#[test]
fn the_served_index_answers_as_the_session_for_every_hash() {
    let (net, r, tee) = play();
    let session = &net.nodes[2].session;
    let mut hashes = tee.seen.lock().expect("seen").clone();
    hashes.extend(r.contributions.iter().map(|qc| *qc.session_hash()));
    hashes.extend([
        *r.complaint.session_hash(),
        *r.justification.session_hash(),
        *r.pcommit.session_hash(),
        [0; 32],
    ]);
    assert_eq!(hashes.len(), 7, "three contributions, one of each other kind and a stranger");
    for h in &hashes {
        same(r.served.contribution(h), session.contribution(h), "contribution", h);
        same(r.served.complaint(h), session.complaint(h), "complaint", h);
        same(r.served.justification(h), session.justification(h), "justification", h);
        same(
            r.served.premature_commitment(h),
            session.get_premature_commitment(h),
            "premature commitment",
            h,
        );
    }
    let answered = |h: &[u8; 32]| {
        session.contribution(h).is_some()
            || session.complaint(h).is_some()
            || session.justification(h).is_some()
            || session.get_premature_commitment(h).is_some()
    };
    assert_eq!(hashes.iter().filter(|h| answered(h)).count(), 6);
}
