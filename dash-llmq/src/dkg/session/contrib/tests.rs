//! The contribution phase (LQ4a-1 T1): `contribute`'s draws and output, `ReceiveMessage` for a
//! contribution, and `VerifyPendingContributions` with its 32-entry trigger (M2).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use super::*;
use crate::dkg::bls::{
    BlsIesMultiBlobs, BlsSchemeKind, BlsSigValue, IesSeed, verify_contribution_share,
    verify_insecure, verify_verification_vector,
};
use crate::dkg::reference::{DebugUpdate, TestMember, TestOperatorKey, quorum};
use crate::dkg::rng::{CoreRng, DkgRng, RangeDraw, SeededRng};
use crate::dkg::session::fixture::{
    BLS, TestEnv, contributions, member_session, quorum_33, test_params,
};
use crate::dkg::session::{DkgBlsRunner, DkgEvent, OperatorKey, SerialRunner};
use crate::dkg::sim::SimErrorRates;

/// The draws a session makes, by tag, with each decision.
#[derive(Clone, Default)]
struct Tape(Rc<RefCell<Vec<String>>>);

impl Tape {
    fn take(&self) -> Vec<String> {
        std::mem::take(&mut self.0.borrow_mut())
    }
}

struct Recording {
    inner: CoreRng<SeededRng>,
    tape: Tape,
}

impl DkgRng for Recording {
    fn new_key(&mut self, why: KeyDraw) -> BlsSkValue {
        self.tape.0.borrow_mut().push(format!("Key({why:?})"));
        self.inner.new_key(why)
    }

    fn ies_seed(&mut self, recipients: usize) -> IesSeed {
        self.tape.0.borrow_mut().push(format!("Ies({recipients})"));
        self.inner.ies_seed(recipients)
    }

    fn rand_bool(&mut self, percent: u8, why: SimError) -> bool {
        let decision = self.inner.rand_bool(percent, why);
        self.tape.0.borrow_mut().push(format!("Bool({why:?})={decision}"));
        decision
    }

    fn rand_range(&mut self, n: u64, why: RangeDraw) -> u64 {
        self.tape.0.borrow_mut().push(format!("Range({why:?})"));
        self.inner.rand_range(n, why)
    }
}

fn recorded(env: &mut TestEnv, seed: u64) -> Tape {
    let tape = Tape::default();
    env.rng = Box::new(Recording {
        inner: CoreRng(SeededRng::new(seed)),
        tape: tape.clone(),
    });
    tape
}

/// An operator key whose decryption always fails.
struct NoDecrypt(TestOperatorKey);

impl OperatorKey for NoDecrypt {
    fn sign(&self, hash: &[u8; 32], legacy: bool) -> BlsSigValue {
        self.0.sign(hash, legacy)
    }

    fn decrypt_share(
        &self,
        _: &BlsIesMultiBlobs,
        _: usize,
        _: BlsSchemeKind,
    ) -> Option<BlsSkValue> {
        None
    }
}

/// An operator key that records the scheme flag it signs under.
struct FlagKey(TestOperatorKey, Rc<Cell<Option<bool>>>);

impl OperatorKey for FlagKey {
    fn sign(&self, hash: &[u8; 32], legacy: bool) -> BlsSigValue {
        self.1.set(Some(legacy));
        self.0.sign(hash, legacy)
    }

    fn decrypt_share(
        &self,
        blobs: &BlsIesMultiBlobs,
        idx: usize,
        flag: BlsSchemeKind,
    ) -> Option<BlsSkValue> {
        self.0.decrypt_share(blobs, idx, flag)
    }
}

/// A runner that counts its calls, then returns `fixed` or the serial verdicts.
struct CountingRunner {
    calls: Rc<Cell<usize>>,
    fixed: Option<Vec<bool>>,
}

impl DkgBlsRunner for CountingRunner {
    fn verify_contribution_shares(
        &self,
        for_id: &[u8; 32],
        vvecs: &[&[BlsPkValue]],
        shares: &[&BlsSkValue],
        flag: BlsSchemeKind,
    ) -> Vec<bool> {
        self.calls.set(self.calls.get() + 1);
        match &self.fixed {
            Some(fixed) => fixed.clone(),
            None => SerialRunner.verify_contribution_shares(for_id, vvecs, shares, flag),
        }
    }
}

fn texts(events: &[DkgEvent]) -> Vec<String> {
    events.iter().map(ToString::to_string).collect()
}

fn rows(env: &TestEnv, kind: &str) -> usize {
    env.store.writers(kind).len()
}

#[test]
fn contribute_draws_in_cores_order() {
    let params = test_params(4);
    let (members, keys) = quorum(4, 1);
    let mut session = member_session(&params, &members, Some(1), true);
    let mut env = TestEnv::new(&keys[1], 2);
    env.sim.set(SimError::ContributionOmit, 1).unwrap();
    env.sim.set(SimError::ContributionLie, 50).unwrap();
    let tape = recorded(&mut env, 3);
    assert!(session.contribute(&mut env.env()).unwrap().is_some());

    let tape = tape.take();
    assert_eq!(
        tape[..4],
        ["Key(Coefficient(0))", "Key(Coefficient(1))", "Bool(ContributionOmit)=false", "Ies(4)"]
    );
    let mut rest = tape[4..].iter();
    let mut lies = 0;
    for i in [0, 2, 3] {
        let decision = rest.next().expect("a lie decision per other member");
        if decision == "Bool(ContributionLie)=true" {
            lies += 1;
            assert_eq!(rest.next().unwrap(), &format!("Key(ContributionLie({i}))"));
        } else {
            assert_eq!(decision, "Bool(ContributionLie)=false");
        }
    }
    assert!(rest.next().is_none());
    assert_eq!(
        texts(&session.take_events())
            .iter()
            .filter(|t| t.starts_with("SendContributions -- lying for"))
            .count(),
        lies
    );
}

#[test]
fn an_omitted_contribution_draws_nothing_after_the_decision() {
    let params = test_params(4);
    let (members, keys) = quorum(4, 1);
    let mut session = member_session(&params, &members, Some(1), true);
    let mut env = TestEnv::new(&keys[1], 2);
    env.sim.set(SimError::ContributionOmit, 100).unwrap();
    let tape = recorded(&mut env, 3);
    assert!(session.contribute(&mut env.env()).unwrap().is_none());

    assert_eq!(tape.take().last().unwrap(), "Bool(ContributionOmit)=true");
    assert!(
        !env.debug.updates.contains(&DebugUpdate::Session(SessionStatusBit::SentContributions))
    );
    assert_eq!(
        texts(&session.take_events()),
        [
            "Contribute -- generating contributions",
            "Contribute -- generated contributions",
            "SendContributions -- sending contributions",
            "SendContributions -- omitting",
        ]
    );
    assert_eq!(session.own_shares.len(), 4, "the polynomial stays in the session");
}

#[test]
fn a_lie_replaces_only_other_members_shares_and_the_true_shares_stay() {
    let params = test_params(4);
    let (members, keys) = quorum(4, 1);
    let mut session = member_session(&params, &members, Some(1), true);
    let mut env = TestEnv::new(&keys[1], 2);
    env.sim.set(SimError::ContributionLie, 100).unwrap();
    let qc = session.contribute(&mut env.env()).unwrap().unwrap();

    for (j, key) in keys.iter().enumerate() {
        let id = &session.member_ids[j];
        assert!(verify_contribution_share(
            id,
            &session.own_vvec,
            &session.own_shares[j],
            BLS.scheme()
        ));
        let sent = key.decrypt_share(qc.blobs(), j, BLS.scheme()).expect("decrypts");
        let fits = verify_contribution_share(id, qc.vvec(), &sent, BLS.scheme());
        assert_eq!(fits, j == 1, "member {j}");
        assert_eq!(sent == session.own_shares[j], j == 1, "member {j}");
    }
}

#[test]
fn an_honest_contribution_is_signed_consistent_and_reads_back() {
    let params = test_params(4);
    let (members, keys) = quorum(4, 1);
    let mut session = member_session(&params, &members, Some(2), true);
    let mut env = TestEnv::new(&keys[2], 2);
    let qc = session.contribute(&mut env.env()).unwrap().unwrap();

    assert!(verify_verification_vector(qc.vvec(), BLS.scheme()));
    for (j, key) in keys.iter().enumerate() {
        let share = key.decrypt_share(qc.blobs(), j, BLS.scheme()).expect("decrypts");
        assert!(verify_contribution_share(&session.member_ids[j], qc.vvec(), &share, BLS.scheme()));
    }
    assert!(verify_insecure(qc.sig(), &members[2].operator_pk, qc.sign_hash(), BLS.scheme()));
    let read = Contribution::read(&qc.payload(BLS.scheme()), &params, BLS).expect("reads");
    assert_eq!(read.session_hash(), qc.session_hash());
    assert_eq!(read.sign_hash(), qc.sign_hash());
    assert_eq!(env.debug.updates, [DebugUpdate::Session(SessionStatusBit::SentContributions)]);
}

#[test]
fn an_invalid_recipient_key_stops_the_contribution_at_that_member() {
    let params = test_params(4);
    let (mut members, keys) = quorum(4, 1);
    members[2].operator_pk = BlsPkValue::invalid();
    let mut session = member_session(&params, &members, Some(1), true);
    let mut env = TestEnv::new(&keys[1], 2);
    env.sim.set(SimError::ContributionLie, 100).unwrap();
    let tape = recorded(&mut env, 3);
    assert!(session.contribute(&mut env.env()).unwrap().is_none());

    assert_eq!(tape.take().last().unwrap(), "Key(ContributionLie(2))");
    let who = members[2].pro_tx_hash;
    assert_eq!(
        texts(&session.take_events()).last().unwrap(),
        &format!("SendContributions -- failed to encrypt contribution for {who}")
    );
    assert!(env.debug.updates.is_empty());
}

#[test]
fn the_signature_is_legacy_iff_v19_is_not_active_after_the_base() {
    let params = test_params(4);
    let (members, keys) = quorum(4, 1);
    for v19 in [true, false] {
        let mut session = member_session(&params, &members, Some(0), v19);
        let mut env = TestEnv::new(&keys[0], 2);
        let flag = Rc::new(Cell::new(None));
        env.key = Box::new(FlagKey(keys[0].clone(), flag.clone()));
        session.contribute(&mut env.env()).unwrap().unwrap();
        assert_eq!(flag.get(), Some(!v19));
    }
}

#[test]
fn contribute_runs_once() {
    let params = test_params(4);
    let (members, keys) = quorum(4, 1);
    let mut env = TestEnv::new(&keys[0], 2);
    let mut member = member_session(&params, &members, Some(0), true);
    assert!(member.contribute(&mut env.env()).unwrap().is_some());
    assert_eq!(member.contribute(&mut env.env()).unwrap_err(), DkgFault::PhaseOrder);
    let mut observer = member_session(&params, &members, None, true);
    assert!(observer.contribute(&mut env.env()).unwrap().is_none());
    assert_eq!(observer.contribute(&mut env.env()).unwrap_err(), DkgFault::PhaseOrder);
}

#[test]
fn an_observer_keeps_the_vvec_writes_nothing_and_relays() {
    let params = test_params(4);
    let (members, keys) = quorum(4, 1);
    let qc = contributions(&params, &members, &keys, [3]).remove(0);
    let mut session = member_session(&params, &members, None, true);
    let mut env = TestEnv::new(&keys[0], 2);
    let inv = session.receive_contribution(qc.clone(), &mut env.env()).expect("relayed");

    assert_eq!(
        inv,
        DkgInv {
            kind: DkgMsgKind::Contribution,
            hash: *qc.session_hash()
        }
    );
    assert_eq!(session.member_state[3].received_vvec.as_deref(), Some(qc.vvec()));
    assert!(env.store.rows.is_empty());
    assert!(session.pending.is_empty());
    assert_eq!(
        session.contribution(qc.session_hash()).map(Contribution::session_hash),
        Some(qc.session_hash())
    );
    let who = members[3].pro_tx_hash;
    assert_eq!(
        texts(&session.take_events()),
        [
            format!("ReceiveMessage -- received contribution from {who}"),
            "ReceiveMessage -- received and relayed contribution. received=1/4".into(),
        ]
    );
    assert_eq!(env.debug.updates, [DebugUpdate::Member(3, MemberStatusBit::ReceivedContribution)]);
}

#[test]
fn a_member_writes_the_vvec_and_queues_each_decrypted_share_its_own_included() {
    let params = test_params(4);
    let (members, keys) = quorum(4, 1);
    let mut session = member_session(&params, &members, Some(1), true);
    let mut env = TestEnv::new(&keys[1], 2);
    let own = session.contribute(&mut env.env()).unwrap().unwrap();
    let other = contributions(&params, &members, &keys, [3]).remove(0);
    session.receive_contribution(own, &mut env.env()).unwrap();
    session.receive_contribution(other, &mut env.env()).unwrap();

    assert_eq!(env.store.writers("V"), [members[1].pro_tx_hash, members[3].pro_tx_hash]);
    assert_eq!(session.pending, [1, 3]);
    assert!(session.member_state[3].received_sk.is_some());
    assert!(session.member_state[3].encrypted.is_some());
    assert!(
        texts(&session.take_events())
            .contains(&"ReceiveMessage -- decrypted our contribution share".into())
    );
}

#[test]
fn a_share_that_does_not_decrypt_is_complained_about_without_a_lie_draw() {
    let params = test_params(4);
    let (members, keys) = quorum(4, 1);
    let qc = contributions(&params, &members, &keys, [3]).remove(0);
    let mut session = member_session(&params, &members, Some(1), true);
    let mut env = TestEnv::new(&keys[1], 2);
    env.key = Box::new(NoDecrypt(keys[1].clone()));
    env.sim.set(SimError::ComplainLie, 100).unwrap();
    let tape = recorded(&mut env, 3);
    assert!(session.receive_contribution(qc, &mut env.env()).is_some());

    assert!(tape.take().is_empty());
    assert!(session.member_state[3].we_complain);
    assert!(session.pending.is_empty());
    assert_eq!(
        env.debug.updates.last(),
        Some(&DebugUpdate::Member(3, MemberStatusBit::WeComplain))
    );
    let who = members[3].pro_tx_hash;
    assert_eq!(
        texts(&session.take_events()).last().unwrap(),
        &format!("ReceiveMessage -- contribution from {who} could not be decrypted")
    );
}

#[test]
fn complain_lie_draws_only_for_another_members_decrypted_share() {
    let params = test_params(4);
    let (members, keys) = quorum(4, 1);
    let mut session = member_session(&params, &members, Some(1), true);
    let mut env = TestEnv::new(&keys[1], 2);
    let own = session.contribute(&mut env.env()).unwrap().unwrap();
    let other = contributions(&params, &members, &keys, [2]).remove(0);
    env.sim.set(SimError::ComplainLie, 100).unwrap();
    let tape = recorded(&mut env, 3);

    session.receive_contribution(own, &mut env.env()).unwrap();
    assert!(tape.take().is_empty());
    session.receive_contribution(other, &mut env.env()).unwrap();
    assert_eq!(tape.take(), ["Bool(ComplainLie)=true"]);
    assert_eq!(session.pending, [1]);
    assert!(session.member_state[2].we_complain);
    let who = members[2].pro_tx_hash;
    assert!(
        texts(&session.take_events())
            .contains(&format!("ReceiveMessage -- lying/complaining for {who}"))
    );
}

#[test]
fn a_second_distinct_contribution_marks_the_member_bad_and_a_third_is_dropped() {
    let params = test_params(4);
    let (members, keys) = quorum(4, 1);
    let first = contributions(&params, &members, &keys, [2]).remove(0);
    let alt = |seed| {
        let mut s = member_session(&params, &members, Some(2), true);
        s.contribute(&mut TestEnv::new(&keys[2], seed).env()).unwrap().unwrap()
    };
    let (second, third) = (alt(0x51), alt(0x52));
    let mut session = member_session(&params, &members, Some(1), true);
    let mut env = TestEnv::new(&keys[1], 2);

    assert!(session.receive_contribution(first, &mut env.env()).is_some());
    let inv = session.receive_contribution(second.clone(), &mut env.env()).expect("relayed");
    assert_eq!(inv.hash, *second.session_hash());
    assert!(session.receive_contribution(third, &mut env.env()).is_none());

    assert!(session.member_state[2].bad);
    assert_eq!(session.pending, [2], "only the first is processed");
    assert_eq!(env.store.writers("V").len(), 1);
    let bad = DebugUpdate::Member(2, MemberStatusBit::Bad);
    assert_eq!(env.debug.updates.iter().filter(|u| **u == bad).count(), 1);
    let who = members[2].pro_tx_hash;
    let events = texts(&session.take_events());
    assert!(events.contains(&format!("ReceiveMessage -- {who} did send multiple contributions")));
    assert_eq!(
        events.last().unwrap(),
        &format!("ReceiveMessage -- received contribution from {who}")
    );
}

#[test]
fn the_32nd_push_verifies_inside_its_receive_and_logs_first() {
    let (members, keys, all) = quorum_33();
    let mut session = member_session(&test_params(33), members, Some(0), true);
    let mut env = TestEnv::new(&keys[0], 2);
    for qc in &all[..31] {
        session.receive_contribution(qc.clone(), &mut env.env()).unwrap();
    }
    assert_eq!((rows(&env, "E"), rows(&env, "S"), session.pending.len()), (0, 0, 31));
    session.take_events();

    session.receive_contribution(all[31].clone(), &mut env.env()).unwrap();
    assert_eq!((rows(&env, "E"), rows(&env, "S"), session.pending.len()), (32, 32, 0));
    let events = texts(&session.take_events());
    assert_eq!(events[0], "VerifyPendingContributions -- verified 32 pending contributions");
    assert_eq!(
        events[1],
        format!("ReceiveMessage -- received contribution from {}", members[31].pro_tx_hash)
    );
    assert_eq!(events.last().unwrap(), "ReceiveMessage -- decrypted our contribution share");

    session.receive_contribution(all[32].clone(), &mut env.env()).unwrap();
    session.verify_pending_contributions(&mut env.env());
    assert_eq!((rows(&env, "E"), rows(&env, "S"), session.pending.len()), (33, 33, 0));
    assert_eq!(
        texts(&session.take_events()).last().unwrap(),
        "VerifyPendingContributions -- verified 1 pending contributions"
    );
}

#[test]
fn shares_that_are_not_pushed_delay_the_trigger() {
    let (members, keys, all) = quorum_33();
    let mut session = member_session(&test_params(33), members, Some(0), true);
    let mut env = TestEnv::new(&keys[0], 2);
    env.key = Box::new(NoDecrypt(keys[0].clone()));
    session.receive_contribution(all[1].clone(), &mut env.env()).unwrap();
    env.key = Box::new(keys[0].clone());
    env.sim.set(SimError::ComplainLie, 100).unwrap();
    session.receive_contribution(all[2].clone(), &mut env.env()).unwrap();
    env.sim.set(SimError::ComplainLie, 0).unwrap();
    for qc in all.iter().skip(3).chain([&all[0]]) {
        session.receive_contribution(qc.clone(), &mut env.env()).unwrap();
    }
    assert_eq!(session.pending.len(), 31);
    assert_eq!(rows(&env, "S"), 0);
}

#[test]
fn a_member_made_bad_after_its_push_gets_no_row() {
    let (members, keys, all) = quorum_33();
    let params = test_params(33);
    let mut again = member_session(&params, members, Some(5), true);
    let second = again.contribute(&mut TestEnv::new(&keys[5], 0x55).env()).unwrap().unwrap();
    let mut session = member_session(&params, members, Some(0), true);
    let mut env = TestEnv::new(&keys[0], 2);
    session.receive_contribution(all[5].clone(), &mut env.env()).unwrap();
    session.receive_contribution(second, &mut env.env()).unwrap();
    for qc in all.iter().enumerate().filter(|(i, _)| *i != 5).map(|(_, qc)| qc).take(31) {
        session.receive_contribution(qc.clone(), &mut env.env()).unwrap();
    }

    assert!(session.pending.is_empty());
    assert_eq!(rows(&env, "E"), 31);
    assert_eq!(rows(&env, "S"), 31);
    assert!(!env.store.writers("E").contains(&members[5].pro_tx_hash));
    assert!(
        texts(&session.take_events())
            .contains(&"VerifyPendingContributions -- verified 32 pending contributions".into())
    );
}

#[test]
fn nothing_left_after_filtering_clears_without_a_verify() {
    let (members, keys, all) = quorum_33();
    let mut session = member_session(&test_params(33), members, Some(0), true);
    let mut env = TestEnv::new(&keys[0], 2);
    let calls = Rc::new(Cell::new(0));
    env.runner = Box::new(CountingRunner {
        calls: calls.clone(),
        fixed: None,
    });
    session.receive_contribution(all[3].clone(), &mut env.env()).unwrap();
    session.mark_bad(3, &mut env.debug);
    session.take_events();
    session.verify_pending_contributions(&mut env.env());

    assert_eq!(calls.get(), 0);
    assert!(session.pending.is_empty());
    assert_eq!(rows(&env, "E"), 0);
    assert_eq!(
        texts(&session.take_events()),
        ["VerifyPendingContributions -- all 1 pending contributions were from bad or \
             complained-about members, nothing to verify"]
    );
}

#[test]
fn a_verdict_count_mismatch_returns_without_clearing() {
    let (members, keys, all) = quorum_33();
    let mut session = member_session(&test_params(33), members, Some(0), true);
    let mut env = TestEnv::new(&keys[0], 2);
    env.runner = Box::new(CountingRunner {
        calls: Rc::default(),
        fixed: Some(Vec::new()),
    });
    session.receive_contribution(all[3].clone(), &mut env.env()).unwrap();
    session.take_events();
    session.verify_pending_contributions(&mut env.env());

    assert_eq!(session.pending, [3]);
    assert_eq!((rows(&env, "E"), rows(&env, "S")), (1, 0));
    assert_eq!(
        texts(&session.take_events()),
        ["VerifyPendingContributions -- VerifyContributionShares returned result of size 0 but \
             size 1 was expected, something is wrong"]
    );
}

#[test]
fn one_invalid_share_among_32_is_the_only_complaint() {
    let (members, keys, all) = quorum_33();
    let mut session = member_session(&test_params(33), members, Some(0), true);
    let mut env = TestEnv::new(&keys[0], 2);
    for qc in &all[..31] {
        session.receive_contribution(qc.clone(), &mut env.env()).unwrap();
    }
    // Pending entry 12 sits in the second batch of 8: replace its share with member 13's
    // share of another polynomial.
    let tampered = session.pending[12];
    session.member_state[tampered].received_sk = session.member_state[13].received_sk.clone();
    session.receive_contribution(all[31].clone(), &mut env.env()).unwrap();

    let complained: Vec<usize> = (0..33).filter(|&i| session.member_state[i].we_complain).collect();
    assert_eq!(complained, [tampered]);
    assert_eq!(rows(&env, "S"), 31);
    assert!(!env.store.writers("S").contains(&members[tampered].pro_tx_hash));
    let who = members[tampered].pro_tx_hash;
    assert!(texts(&session.take_events()).contains(&format!(
        "VerifyPendingContributions -- invalid contribution from {who}. will complain later"
    )));
}

/// A member session as member 1 of four that has received member 2's contribution twice, as an
/// equal session hash re-delivered (m6 (d)); the second time under `sim`.
fn received_twice(sim: SimErrorRates) -> (DkgSession<TestMember>, TestEnv, ProTxHash) {
    let params = test_params(4);
    let (members, keys) = quorum(4, 1);
    let qc = contributions(&params, &members, &keys, [2]).remove(0);
    let mut session = member_session(&params, &members, Some(1), true);
    let mut env = TestEnv::new(&keys[1], 2);
    session.receive_contribution(qc.clone(), &mut env.env()).expect("relayed");
    env.sim = sim;
    session.receive_contribution(qc, &mut env.env()).expect("relayed again");
    assert_eq!(session.member_state[2].contributions.len(), 1);
    (session, env, members[2].pro_tx_hash)
}

#[test]
fn an_equal_session_hash_re_delivered_to_a_member_is_written_and_queued_again() {
    // Core processes it again (`l/dkgsession.cpp:234`, :260): a second `qdkg_V` row and a second
    // push, and the pending verify writes a `qdkg_E` row for each entry (`a/dkgsession.cpp:136-143`).
    let (mut session, mut env, who) = received_twice(SimErrorRates::default());
    assert_eq!(session.pending, [2, 2]);
    assert_eq!(env.store.writers("V"), [who, who]);
    session.verify_pending_contributions(&mut env.env());
    assert_eq!(env.store.writers("E"), [who, who]);
    assert!(session.pending.is_empty());
}

#[test]
fn a_complaint_on_the_re_delivery_skips_the_share_already_queued() {
    // The second processing lies about the member while its first push waits, so the pending
    // verify filters it out (`a/dkgsession.cpp:138`).
    let mut sim = SimErrorRates::default();
    sim.set(SimError::ComplainLie, 100).unwrap();
    let (mut session, mut env, _) = received_twice(sim);
    assert!(session.member_state[2].we_complain);
    assert_eq!(session.pending, [2]);
    session.take_events();
    session.verify_pending_contributions(&mut env.env());
    assert!(env.store.writers("E").is_empty());
    assert!(env.store.writers("S").is_empty());
    assert_eq!(
        texts(&session.take_events()),
        ["VerifyPendingContributions -- all 1 pending contributions were from bad or \
             complained-about members, nothing to verify"]
    );
}
