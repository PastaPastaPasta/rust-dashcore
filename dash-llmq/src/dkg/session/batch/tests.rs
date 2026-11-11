//! `ProcessPendingMessageBatch` (LQ4a-1 T1): the batch signature check, the m6 rows, score
//! multiplicity, and the 32-entry verify inside a pop (M2).

use std::cell::RefCell;
use std::convert::Infallible;
use std::rc::Rc;

use dash_evo::types::ProTxHash;
use getrandom::SysRng;
use rand_core::{Rng, TryCryptoRng, TryRng, UnwrapErr};

use super::*;
use crate::dkg::bls::{
    BlsSchemeKind, BlsSkValue, IesSeed, add_and_subtract_error, make_new_key, secret_to_public,
    sign,
};
use crate::dkg::reference::{TestMember, TestOperatorKey, quorum};
use crate::dkg::rng::{DkgRng, KeyDraw, RangeDraw, SeededRng};
use crate::dkg::session::OperatorKey;
use crate::dkg::session::fixture::{
    BLS, TestEnv, contributions, member_session, quorum_33, test_params, valid_sig_bytes,
};
use crate::dkg::sim::SimError;

type Batch<'a> = Vec<(Sender<u32>, &'a [u8])>;

/// `qc`'s payload with its signature replaced by a valid signature over another hash.
fn forged(qc: &Contribution) -> Vec<u8> {
    let mut payload = qc.payload(BLS.scheme());
    let at = payload.len() - 96;
    payload[at..].copy_from_slice(&valid_sig_bytes());
    payload
}

/// `qc`'s payload from a proTxHash that is not a member.
fn from_outsider(qc: &Contribution) -> Vec<u8> {
    let mut payload = qc.payload(BLS.scheme());
    payload[33..65].copy_from_slice(&[0xee; 32]);
    payload
}

fn texts(outcome: &BatchOutcome<u32>) -> Vec<String> {
    outcome.events.iter().map(ToString::to_string).collect()
}

fn scores(outcome: &BatchOutcome<u32>) -> Vec<Misbehaviour<u32>> {
    outcome.misbehaving.iter().map(|s| s.misbehaviour).collect()
}

fn hashes(outcome: &BatchOutcome<u32>) -> Vec<[u8; 32]> {
    outcome.relay.iter().map(|r| r.inv.hash).collect()
}

/// Each record's lines are contiguous, and each relay follows the last line of the
/// `ReceiveMessage` record that returned its inv.
fn assert_records_and_relays(outcome: &BatchOutcome<u32>) {
    let events = &outcome.events;
    let mut closed = BTreeSet::new();
    for (i, e) in events.iter().enumerate() {
        let Some(id) = e.record else {
            continue;
        };
        if i > 0 && events[i - 1].record != Some(id) {
            assert!(closed.insert(id), "record {id} is split at line {i}");
        }
    }
    for scored in &outcome.misbehaving {
        let peer = match scored.misbehaviour {
            Misbehaviour::Decode {
                peer,
                ..
            }
            | Misbehaviour::PreVerifyBan {
                peer,
                ..
            }
            | Misbehaviour::BadSignature {
                peer,
            } => peer,
        };
        let line = &events[scored.at - 1];
        assert_eq!(line.record, None, "score at {}", scored.at);
        assert!(line.text.ends_with(&format!("peer={peer}")), "score at {}", scored.at);
    }
    for r in &outcome.relay {
        let last = &events[r.at - 1];
        assert_eq!(last.func, "ReceiveMessage", "relay at {}", r.at);
        assert!(last.record.is_some());
        assert!(events.get(r.at).is_none_or(|next| next.record != last.record));
    }
}

/// A four-member LLMQ_TEST session as member 1, with the honest contributions of members 0, 2
/// and 3.
fn four() -> (DkgSession<TestMember>, Vec<TestMember>, Vec<TestOperatorKey>, Vec<Contribution>) {
    let params = test_params(4);
    let (members, keys) = quorum(4, 1);
    let others = contributions(&params, &members, &keys, [0, 2, 3]);
    let session = member_session(&params, &members, Some(1), true);
    (session, members, keys, others)
}

fn run(
    session: &mut DkgSession<TestMember>,
    env: &mut TestEnv,
    batch: &Batch,
) -> BatchOutcome<u32> {
    session.process_batch(DkgMsgKind::Contribution, batch, &mut env.env())
}

#[test]
fn a_good_batch_is_received_and_relayed_in_order() {
    let (mut session, _, keys, others) = four();
    let mut env = TestEnv::new(&keys[1], 2);
    let own = session.contribute(&mut env.env()).unwrap().unwrap();
    let payloads: Vec<Vec<u8>> =
        [&others[0], &own, &others[1], &others[2]].map(|qc| qc.payload(BLS.scheme())).into();
    let batch: Batch = vec![
        (Sender::Peer(10), &payloads[0]),
        (Sender::Own, &payloads[1]),
        (Sender::Peer(12), &payloads[2]),
        (Sender::Peer(13), &payloads[3]),
    ];
    let outcome = run(&mut session, &mut env, &batch);

    assert!(outcome.misbehaving.is_empty());
    let want = [&others[0], &own, &others[1], &others[2]].map(|qc| *qc.session_hash());
    assert_eq!(hashes(&outcome), want);
    assert!(outcome.relay.iter().all(|r| r.inv.kind == DkgMsgKind::Contribution));
    assert_eq!(session.pending, [0, 1, 2, 3]);

    // `Contribute` flushes, then `SendContributions`, `PreVerifyMessage` and `ReceiveMessage`
    // each batch their own record.
    assert_records_and_relays(&outcome);
    let funcs =
        |record| outcome.events.iter().filter(|e| e.record == Some(record)).map(|e| e.func).next();
    let ids: BTreeSet<u32> = outcome.events.iter().filter_map(|e| e.record).collect();
    assert_eq!(funcs(*ids.first().unwrap()), Some("Contribute"));
    let receives = ids.iter().filter(|&&id| funcs(id) == Some("ReceiveMessage")).count();
    assert_eq!(receives, 4);
}

#[test]
fn a_bad_sender_loses_every_message_it_sent_in_the_batch() {
    let (mut session, _, keys, others) = four();
    let mut env = TestEnv::new(&keys[1], 2);
    let (good, bad, other) =
        (others[0].payload(BLS.scheme()), forged(&others[1]), others[2].payload(BLS.scheme()));
    let batch: Batch =
        vec![(Sender::Peer(9), &good), (Sender::Peer(9), &bad), (Sender::Peer(10), &other)];
    let outcome = run(&mut session, &mut env, &batch);

    assert_eq!(
        scores(&outcome),
        [Misbehaviour::BadSignature {
            peer: 9
        }]
    );
    assert_eq!(hashes(&outcome), [*others[2].session_hash()]);
    assert!(session.member_state[0].contributions.is_empty(), "its valid message is skipped too");
    assert!(
        texts(&outcome)
            .contains(&"ProcessPendingMessageBatch -- failed to verify signature, peer=9".into())
    );
}

#[test]
fn a_failed_aggregate_from_one_sender_condemns_it() {
    let (mut session, _, keys, others) = four();
    let mut env = TestEnv::new(&keys[1], 2);
    let (good, bad) = (others[0].payload(BLS.scheme()), forged(&others[1]));
    let batch: Batch = vec![(Sender::Peer(9), &good), (Sender::Peer(9), &bad)];
    let outcome = run(&mut session, &mut env, &batch);

    assert_eq!(
        scores(&outcome),
        [Misbehaviour::BadSignature {
            peer: 9
        }]
    );
    assert!(outcome.relay.is_empty());
}

#[test]
fn the_one_sender_shortcut_needs_every_message_from_that_sender() {
    // Core takes it only when no two adjacent messages differ in sender (`l/net_dkg.cpp:289-298`):
    // here the aggregate fails, sender 9's two valid messages are kept, and only 10 is scored.
    let (mut session, _, keys, others) = four();
    let mut env = TestEnv::new(&keys[1], 2);
    let (a, b, bad) =
        (others[0].payload(BLS.scheme()), others[1].payload(BLS.scheme()), forged(&others[2]));
    let batch: Batch = vec![(Sender::Peer(9), &a), (Sender::Peer(9), &b), (Sender::Peer(10), &bad)];
    let outcome = run(&mut session, &mut env, &batch);

    assert_eq!(
        scores(&outcome),
        [Misbehaviour::BadSignature {
            peer: 10
        }]
    );
    assert_eq!(hashes(&outcome), [*others[0].session_hash(), *others[1].session_hash()]);
    assert!(session.member_state[3].contributions.is_empty());
    assert_records_and_relays(&outcome);
}

#[test]
fn the_same_pro_tx_on_two_connections_is_two_senders() {
    // m6 (a). The forgery shares the honest message's sign hash, so the batch also takes the
    // duplicate-hash path to single checks.
    let (mut session, _, keys, others) = four();
    let mut env = TestEnv::new(&keys[1], 2);
    let (good, bad) = (others[1].payload(BLS.scheme()), forged(&others[1]));
    let batch: Batch = vec![(Sender::Peer(1), &good), (Sender::Peer(2), &bad)];
    let outcome = run(&mut session, &mut env, &batch);

    assert_eq!(
        scores(&outcome),
        [Misbehaviour::BadSignature {
            peer: 2
        }]
    );
    assert_eq!(hashes(&outcome), [*others[1].session_hash()]);
}

#[test]
fn a_repeated_sign_hash_is_checked_message_by_message() {
    let (mut session, _, keys, others) = four();
    let mut env = TestEnv::new(&keys[1], 2);
    let payload = others[1].payload(BLS.scheme());
    let batch: Batch = vec![(Sender::Peer(1), &payload), (Sender::Peer(2), &payload)];
    let outcome = run(&mut session, &mut env, &batch);

    assert!(outcome.misbehaving.is_empty());
    assert_eq!(hashes(&outcome), [*others[1].session_hash(); 2]);
    assert_eq!(session.member_state[2].contributions.len(), 1);
}

#[test]
fn own_messages_are_never_scored() {
    let (mut session, _, keys, others) = four();
    let mut env = TestEnv::new(&keys[1], 2);
    let (garbage, bad) = (vec![0u8], forged(&others[0]));
    let batch: Batch = vec![(Sender::Own, &garbage), (Sender::Own, &bad)];
    let outcome = run(&mut session, &mut env, &batch);

    assert!(outcome.misbehaving.is_empty());
    assert!(outcome.relay.is_empty());
    let events = texts(&outcome);
    assert!(events.contains(
        &"DeserializeCheckedDKGMessage -- failed to deserialize message, peer=-1".into()
    ));
    assert!(
        events
            .contains(&"ProcessPendingMessageBatch -- failed to verify signature, peer=-1".into())
    );
}

#[test]
fn scores_are_per_message_before_the_signature_check_and_per_sender_after() {
    let (mut session, _, keys, others) = four();
    let mut env = TestEnv::new(&keys[1], 2);
    let garbage = vec![0u8];
    let (outsider_a, outsider_b) = (from_outsider(&others[0]), from_outsider(&others[1]));
    let (bad_a, bad_b) = (forged(&others[0]), forged(&others[2]));
    let batch: Batch = vec![
        (Sender::Peer(1), &garbage),
        (Sender::Peer(1), &garbage),
        (Sender::Peer(3), &outsider_a),
        (Sender::Peer(3), &outsider_b),
        (Sender::Peer(2), &bad_a),
        (Sender::Peer(2), &bad_b),
        (Sender::Own, &garbage),
    ];
    let outcome = run(&mut session, &mut env, &batch);
    assert_records_and_relays(&outcome);

    let decode = Misbehaviour::Decode {
        peer: 1,
        reason: DkgReject::Deserialize,
    };
    let ban = Misbehaviour::PreVerifyBan {
        peer: 3,
        reason: DkgReject::NotMember,
    };
    assert_eq!(
        scores(&outcome),
        [
            decode,
            decode,
            ban,
            ban,
            Misbehaviour::BadSignature {
                peer: 2
            }
        ]
    );
}

#[test]
fn three_distinct_contributions_in_one_batch_relay_two_and_mark_the_member_bad() {
    // m6 (c): PreVerify counts what was recorded before the batch, so all three pass it.
    let (mut session, members, keys, _) = four();
    let params = test_params(4);
    let alt = |seed| {
        let mut s = member_session(&params, &members, Some(2), true);
        s.contribute(&mut TestEnv::new(&keys[2], seed).env()).unwrap().unwrap()
    };
    let qcs = [alt(0x61), alt(0x62), alt(0x63)];
    let payloads = qcs.each_ref().map(|qc| qc.payload(BLS.scheme()));
    let batch: Batch = payloads.iter().map(|p| (Sender::Peer(1), &p[..])).collect();
    let mut env = TestEnv::new(&keys[1], 2);
    let outcome = run(&mut session, &mut env, &batch);

    assert!(outcome.misbehaving.is_empty());
    assert_eq!(hashes(&outcome), [*qcs[0].session_hash(), *qcs[1].session_hash()]);
    assert!(session.member_state[2].bad);
    assert_eq!(session.member_state[2].contributions.len(), 2);
    assert_eq!(session.pending, [2], "only the first is processed");
}

#[test]
fn an_equal_session_hash_in_other_bytes_is_processed_again() {
    // m6 (d). The basic read zeroes an ephemeral key whose legacy layout is canonical only in
    // the legacy scheme, so two such keys give one session hash. PreVerify does not look at
    // the ephemeral key.
    let params = test_params(4);
    let (members, keys) = quorum(4, 1);
    let qc = contributions(&params, &members, &keys, [2]).remove(0);
    let at = 1 + 32 + 32 + 1 + 48 * qc.vvec().len();
    let with_ephemeral = |n: u8| {
        let mut b = [0u8; 32];
        b[31] = n;
        let sk = BlsSkValue::from_canonical(&b, BlsSchemeKind::Basic).expect("small");
        let mut payload = qc.payload(BLS.scheme());
        payload[at..at + 48]
            .copy_from_slice(&secret_to_public(&sk).to_bytes(BlsSchemeKind::Legacy));
        payload
    };
    let zeroed =
        |p: &Vec<u8>| !Contribution::read(p, &params, BLS).unwrap().blobs().ephemeral.is_valid();
    let mut picked = (1..=u8::MAX).map(with_ephemeral).filter(zeroed);
    let (a, b) = (picked.next().unwrap(), picked.next().unwrap());
    assert_ne!(a, b);
    let read = Contribution::read(&a, &params, BLS).expect("reads");
    assert_eq!(read.session_hash(), Contribution::read(&b, &params, BLS).unwrap().session_hash());
    let sig = keys[2].sign(read.sign_hash(), false).to_bytes(BlsSchemeKind::Basic);
    let signed = |mut p: Vec<u8>| {
        let n = p.len();
        p[n - 96..].copy_from_slice(&sig);
        p
    };
    let (a, b) = (signed(a), signed(b));

    let mut session = member_session(&params, &members, None, true);
    let mut env = TestEnv::new(&keys[0], 2);
    let first = run(&mut session, &mut env, &vec![(Sender::Peer(1), &a[..])]);
    let again = run(&mut session, &mut env, &vec![(Sender::Peer(2), &b[..])]);

    assert_eq!(hashes(&first), hashes(&again));
    assert_eq!(session.member_state[2].contributions.len(), 1);
    let processed = "ReceiveMessage -- received and relayed contribution. received=1/4";
    assert!(texts(&first).contains(&processed.into()));
    assert!(texts(&again).contains(&processed.into()));
}

/// A message for [`DkgSession::batch_verify_message_sigs`] alone.
struct Fake {
    signer: ProTxHash,
    sig: BlsSigValue,
    hash: [u8; 32],
}

impl Signed for Fake {
    fn signer(&self) -> &ProTxHash {
        &self.signer
    }
    fn signature(&self) -> &BlsSigValue {
        &self.sig
    }
    fn signed_hash(&self) -> &[u8; 32] {
        &self.hash
    }
}

#[test]
fn invalid_signatures_and_unknown_members_never_reach_the_aggregate() {
    let (session, members, keys, _) = four();
    let hash = [9; 32];
    let good = Fake {
        signer: members[0].pro_tx_hash,
        sig: keys[0].sign(&hash, false),
        hash,
    };
    let invalid = Fake {
        signer: members[2].pro_tx_hash,
        sig: BlsSigValue::invalid(),
        hash: [8; 32],
    };
    let stranger = Fake {
        signer: ProTxHash::from([0xee; 32]),
        sig: keys[3].sign(&[7; 32], false),
        hash: [7; 32],
    };
    let bad = session.batch_verify_message_sigs(
        &[(Sender::Peer(1u32), invalid), (Sender::Peer(2), stranger), (Sender::Peer(3), good)],
        BLS,
        &mut os(),
    );
    assert_eq!(bad.into_iter().collect::<Vec<_>>(), [Sender::Peer(1), Sender::Peer(2)]);

    let none_left = session.batch_verify_message_sigs(
        &[(
            Sender::Peer(1u32),
            Fake {
                signer: members[2].pro_tx_hash,
                sig: BlsSigValue::invalid(),
                hash,
            },
        )],
        BLS,
        &mut os(),
    );
    assert_eq!(none_left.into_iter().collect::<Vec<_>>(), [Sender::Peer(1)]);
}

#[test]
fn a_repeated_sign_hash_from_one_sender_is_checked_message_by_message() {
    // `l/net_dkg.cpp:262-272`: a repeated sign hash sends the batch to single checks before the
    // aggregate is tried. The aggregate over a repeated hash fails, so without that step one
    // sender's two valid messages would take the one-sender shortcut and condemn it.
    let (session, members, keys, _) = four();
    let hash = [9; 32];
    let sig = keys[0].sign(&hash, false);
    let pk = &members[0].operator_pk;
    assert!(!verify_insecure_aggregated(&[&sig, &sig], &[pk, pk], &[hash, hash], BLS.scheme()));
    let message = || Fake {
        signer: members[0].pro_tx_hash,
        sig: sig.clone(),
        hash,
    };
    let draws = Rc::new(RefCell::new(Vec::new()));
    let bad = session.batch_verify_message_sigs(
        &[(Sender::Peer(1u32), message()), (Sender::Peer(1), message())],
        BLS,
        &mut RecordingWeights(draws.clone()),
    );
    assert!(bad.is_empty());
    // v24.0.0 draws the repeating message's weight before its hash check (:262, :275).
    assert_eq!(*draws.borrow(), [32, 32]);
}

#[test]
fn the_32nd_push_mid_pop_verifies_before_the_next_message() {
    // M2: 29 shares pushed, then a pop of 8 whose third message is the 32nd push; four forged
    // messages from another connection fill the pop and are skipped.
    let (members, keys, all) = quorum_33();
    let mut session = member_session(&test_params(33), members, Some(0), true);
    let mut env = TestEnv::new(&keys[0], 2);
    let mut events = Vec::new();
    let payloads: Vec<Vec<u8>> = all.iter().map(|qc| qc.payload(BLS.scheme())).collect();
    let sender = |i: usize| {
        if i == 0 {
            Sender::Own
        } else {
            Sender::Peer(i as u32)
        }
    };
    for pop in [0..8, 8..16, 16..24, 24..29] {
        let batch: Batch = pop.map(|i| (sender(i), &payloads[i][..])).collect();
        let outcome = run(&mut session, &mut env, &batch);
        assert!(outcome.misbehaving.is_empty());
        events.extend(texts(&outcome));
    }
    assert_eq!((env.store.writers("E").len(), session.pending.len()), (0, 29));

    let forgeries: Vec<Vec<u8>> = all[5..9].iter().map(forged).collect();
    let mut batch: Batch = (29..33).map(|i| (sender(i), &payloads[i][..])).collect();
    batch.extend(forgeries.iter().map(|p| (Sender::Peer(99), &p[..])));
    let outcome = run(&mut session, &mut env, &batch);
    assert_eq!(
        scores(&outcome),
        [Misbehaviour::BadSignature {
            peer: 99
        }]
    );
    assert_eq!(outcome.relay.len(), 4);
    assert_eq!((env.store.writers("E").len(), env.store.writers("S").len()), (32, 32));
    assert_eq!(session.pending, [32]);

    let pop = texts(&outcome);
    let at = |text: String| pop.iter().position(|t| *t == text).expect("logged");
    let received = |i: usize| {
        at(format!("ReceiveMessage -- received contribution from {}", members[i].pro_tx_hash))
    };
    let verified = at("VerifyPendingContributions -- verified 32 pending contributions".into());
    assert!(received(30) < verified, "message 2 is received first");
    assert!(verified < received(31), "Core flushes the verify's lines before its caller's");
    assert!(verified < received(32));
    assert_records_and_relays(&outcome);
    let record = |i: usize| outcome.events[i].record;
    assert_ne!(record(verified), record(received(31)), "the verify has a logger of its own");
    events.extend(pop);

    session.verify_pending_contributions(&mut env.env());
    assert_eq!((env.store.writers("E").len(), env.store.writers("S").len()), (33, 33));
    let drain = texts(&BatchOutcome {
        events: session.take_events(),
        ..BatchOutcome::default()
    });
    assert_eq!(drain, ["VerifyPendingContributions -- verified 1 pending contributions"]);
    events.extend(drain);

    assert!(events.iter().all(|t| !t.contains("time")), "no cxxtimer fields");
}

/// The OS RNG for the batch weights.
fn os() -> UnwrapErr<SysRng> {
    UnwrapErr(SysRng)
}

/// The OS RNG, recording the length in bytes of each draw made from it.
struct RecordingWeights(Rc<RefCell<Vec<usize>>>);

impl TryRng for RecordingWeights {
    type Error = Infallible;

    fn try_next_u32(&mut self) -> Result<u32, Infallible> {
        self.0.borrow_mut().push(4);
        Ok(os().next_u32())
    }

    fn try_next_u64(&mut self) -> Result<u64, Infallible> {
        self.0.borrow_mut().push(8);
        Ok(os().next_u64())
    }

    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Infallible> {
        self.0.borrow_mut().push(dst.len());
        os().fill_bytes(dst);
        Ok(())
    }
}

impl TryCryptoRng for RecordingWeights {}

/// A session RNG that panics on any draw.
struct NoDraws;

impl DkgRng for NoDraws {
    fn new_key(&mut self, why: KeyDraw) -> BlsSkValue {
        panic!("a session draw: {why:?}")
    }

    fn ies_seed(&mut self, recipients: usize) -> IesSeed {
        panic!("a session draw: IES seed for {recipients}")
    }

    fn rand_bool(&mut self, _: u8, why: SimError) -> bool {
        panic!("a session draw: {why:?}")
    }

    fn rand_range(&mut self, _: u64, why: RangeDraw) -> u64 {
        panic!("a session draw: {why:?}")
    }
}

/// `a + E` and `b - E` for `E` a signature over `uint256::ONE` under a fresh key, as Core's
/// `AddCancellingMessages` (`test/bls_tests.cpp:294-316`, v24.0.0) builds them: each is wrong,
/// and their unweighted sum is `a + b`.
fn cancelling(a: &BlsSigValue, b: &BlsSigValue) -> (BlsSigValue, BlsSigValue) {
    let mut one = [0; 32];
    one[0] = 1;
    let error = sign(&make_new_key(&mut SeededRng::new(0xe7)), &one, false);
    add_and_subtract_error(a, b, &error)
}

#[test]
fn errors_that_cancel_across_two_senders_condemn_both() {
    // v24.0.0 `l/net_dkg.cpp:262-271`: each signature and operator key enter the aggregate times
    // a weight of their own, so the two wrong signatures, whose unweighted sum verifies, fail the
    // aggregate, and the per-message checks find both.
    let (session, members, keys, _) = four();
    let (h0, h2, h3) = ([1; 32], [2; 32], [3; 32]);
    let (s0, s2) = cancelling(&keys[0].sign(&h0, false), &keys[2].sign(&h2, false));
    let (pk0, pk2) = (&members[0].operator_pk, &members[2].operator_pk);
    assert!(verify_insecure_aggregated(&[&s0, &s2], &[pk0, pk2], &[h0, h2], BLS.scheme()));
    assert!(!verify_insecure(&s0, pk0, &h0, BLS.scheme()));
    assert!(!verify_insecure(&s2, pk2, &h2, BLS.scheme()));
    let message = |i: usize, sig: BlsSigValue, hash: [u8; 32]| Fake {
        signer: members[i].pro_tx_hash,
        sig,
        hash,
    };
    let bad = session.batch_verify_message_sigs(
        &[
            (Sender::Peer(3u32), message(3, keys[3].sign(&h3, false), h3)),
            (Sender::Peer(1), message(0, s0, h0)),
            (Sender::Peer(2), message(2, s2, h2)),
        ],
        BLS,
        &mut os(),
    );
    assert_eq!(bad.into_iter().collect::<Vec<_>>(), [Sender::Peer(1), Sender::Peer(2)]);
}

#[test]
fn errors_that_cancel_across_one_senders_messages_condemn_it() {
    // The same pair from one sender under one key: the weighted aggregate fails and every
    // message came from that sender, so it is bad. Its two honest messages pass.
    let (session, members, keys, _) = four();
    let (h1, h2) = ([1; 32], [2; 32]);
    let message = |sig: BlsSigValue, hash: [u8; 32]| {
        (
            Sender::Peer(1u32),
            Fake {
                signer: members[0].pro_tx_hash,
                sig,
                hash,
            },
        )
    };
    let (s1, s2) = (keys[0].sign(&h1, false), keys[0].sign(&h2, false));
    let honest = [message(s1.clone(), h1), message(s2.clone(), h2)];
    assert!(session.batch_verify_message_sigs(&honest, BLS, &mut os()).is_empty());

    let (w1, w2) = cancelling(&s1, &s2);
    let pk = &members[0].operator_pk;
    assert!(verify_insecure_aggregated(&[&w1, &w2], &[pk, pk], &[h1, h2], BLS.scheme()));
    let bad =
        session.batch_verify_message_sigs(&[message(w1, h1), message(w2, h2)], BLS, &mut os());
    assert_eq!(bad.into_iter().collect::<Vec<_>>(), [Sender::Peer(1)]);
}

#[test]
fn an_invalid_operator_key_marks_its_sender_and_draws_no_weight() {
    // v24.0.0 `l/net_dkg.cpp:255-260`: the key is checked before a weight is drawn (:262), and
    // the other messages are judged without it.
    let (mut members, keys) = quorum(4, 1);
    members[2].operator_pk = BlsPkValue::invalid();
    let session = member_session(&test_params(4), &members, Some(1), true);
    let signed = |i: usize, hash: [u8; 32]| Fake {
        signer: members[i].pro_tx_hash,
        sig: keys[i].sign(&hash, false),
        hash,
    };
    let wrong = Fake {
        hash: [3; 32],
        ..signed(3, [0x33; 32])
    };

    let draws = Rc::new(RefCell::new(Vec::new()));
    let bad = session.batch_verify_message_sigs(
        &[
            (Sender::Peer(0u32), signed(0, [0; 32])),
            (Sender::Peer(2), signed(2, [2; 32])),
            (Sender::Peer(3), wrong),
            (Sender::Peer(1), signed(1, [1; 32])),
        ],
        BLS,
        &mut RecordingWeights(draws.clone()),
    );
    assert_eq!(bad.into_iter().collect::<Vec<_>>(), [Sender::Peer(2), Sender::Peer(3)]);
    assert_eq!(*draws.borrow(), [32, 32, 32]);

    draws.borrow_mut().clear();
    let bad = session.batch_verify_message_sigs(
        &[(Sender::Peer(0u32), signed(0, [0; 32])), (Sender::Peer(2), signed(2, [2; 32]))],
        BLS,
        &mut RecordingWeights(draws.clone()),
    );
    assert_eq!(bad.into_iter().collect::<Vec<_>>(), [Sender::Peer(2)]);
    assert_eq!(*draws.borrow(), [32]);
}

#[test]
fn message_batch_weights_come_from_the_injected_rng_never_the_sessions() {
    // The weights are no session draw: a session RNG that panics on any draw lets the batch
    // finish, and the injected RNG gives one 32-byte draw, for one 16-byte weight, per message
    // that reaches the aggregate.
    let (mut session, _, keys, others) = four();
    let mut env = TestEnv::new(&keys[1], 2);
    env.rng = Box::new(NoDraws);
    let draws = Rc::new(RefCell::new(Vec::new()));
    env.weights = Box::new(RecordingWeights(draws.clone()));
    let payloads: Vec<Vec<u8>> = others.iter().map(|qc| qc.payload(BLS.scheme())).collect();
    let forgery = forged(&others[1]);
    let batch: Batch = vec![
        (Sender::Peer(0), &payloads[0][..]),
        (Sender::Peer(2), &forgery[..]),
        (Sender::Peer(3), &payloads[2][..]),
    ];
    let outcome = run(&mut session, &mut env, &batch);

    assert_eq!(
        scores(&outcome),
        [Misbehaviour::BadSignature {
            peer: 2
        }]
    );
    assert_eq!(outcome.relay.len(), 2);
    assert_eq!(*draws.borrow(), [32, 32, 32]);
}
