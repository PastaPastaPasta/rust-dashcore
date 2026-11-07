//! T4 (LQ4 §4.3, LQ4b-1): whole quorums of the reference harness from contribution to final
//! commitment. Every finalizer's commitment must be byte-equal to every other's, pass
//! `verify_commitment(.., Some(SigPath::Sync))` at the quorum's base (checked again here by a
//! verifier of its own), and have the version, quorumIndex, `validMembers` and signers the run
//! gives. Each phase's members step in parallel, on at most [`THREADS`] threads.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Instant;

use dash_consensus::hash::{sha256, sha256d};
use dash_evo::llmq::LlmqCommitment;
use dash_evo::types::ProTxHash;
use dash_llmq::dkg::bls::{BlsCtx, BlsSchemeKind, BlsSkValue};
use dash_llmq::dkg::conn::ConnSnapshot;
use dash_llmq::dkg::reference::{
    CoreRelayTopology, DebugUpdate, MemStore, Network, NetworkSetup, Node, RecordingDebug,
    TestMember, TestOperatorKey,
};
use dash_llmq::dkg::rng::{CoreRng, SeededRng};
use dash_llmq::dkg::session::{
    DkgEnv, DkgInv, DkgMsgKind, DkgSession, FinalVerifier, MemberStatusBit, Misbehaviour, Sender,
    SerialRunner, SessionInit,
};
use dash_llmq::dkg::sim::{SimError, SimErrorRates};
use dash_llmq::dkg::wire::{QuorumHash, final_commitment_bytes};
use dash_params::{LlmqParams, LlmqType};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;
use serde_json::Value;

/// The most threads a run steps its members on.
const THREADS: usize = 4;

/// Core's static table entry for `t` (`llmq/params.h`).
fn params(t: LlmqType) -> LlmqParams {
    dash_params::available_params(t).expect("a known type")
}

/// `f` on each member of `who` (each once) in parallel, with the network's scheme; the
/// results in `who`'s order.
fn each<R: Send>(
    net: &mut Network,
    who: &[usize],
    f: impl Fn(&mut Node, BlsCtx) -> R + Sync,
) -> Vec<R> {
    let bls = net.bls;
    let mut picked: Vec<(usize, &mut Node)> =
        net.nodes.iter_mut().enumerate().filter(|(i, _)| who.contains(i)).collect();
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, THREADS);
    let chunk = picked.len().div_ceil(threads).max(1);
    let mut out: BTreeMap<usize, R> = std::thread::scope(|s| {
        let f = &f;
        let handles: Vec<_> = picked
            .chunks_mut(chunk)
            .map(|part| {
                s.spawn(move || {
                    part.iter_mut().map(|(i, node)| (*i, f(node, bls))).collect::<Vec<_>>()
                })
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().expect("a member's step")).collect()
    });
    who.iter().map(|i| out.remove(i).expect("each member once")).collect()
}

/// How a run departs from an honest quorum.
#[derive(Default)]
struct Plan {
    /// Members that take no part: they send nothing and their queues are dropped.
    offline: BTreeSet<usize>,
    /// Modified senders, each with the members it encrypts a fresh key to; their contributions
    /// are never lost.
    liars: Vec<(usize, BTreeSet<usize>)>,
    /// `(member, type, rate)`: simulated errors, which only `llmq_test` draws.
    sim: Vec<(usize, SimError, i32)>,
    /// `(phase, member)`: the member's message of that phase is lost on the way, so it
    /// reaches nobody but the member itself.
    lost: Vec<(DkgMsgKind, usize)>,
    /// The members that receive the premature commitments and finalize: every online one when
    /// empty.
    finalizers: Vec<usize>,
    /// `(phase, scheme)`: the process-wide scheme from that phase's start on.
    bls_at: Vec<(DkgMsgKind, BlsCtx)>,
}

impl Plan {
    fn online(&self, n: usize) -> Vec<usize> {
        (0..n).filter(|i| !self.offline.contains(i)).collect()
    }

    /// The start of `phase`: its scheme, if the plan changes it, and its queues.
    fn begin(&self, net: &mut Network, phase: DkgMsgKind) {
        if let Some(&(_, bls)) = self.bls_at.iter().find(|(p, _)| *p == phase) {
            net.set_bls(bls);
        }
        net.enter(phase);
    }
}

/// Each member of `who` with the payload it sent, if any.
fn sent_by(who: &[usize], sent: Vec<Option<Vec<u8>>>) -> Vec<(usize, Vec<u8>)> {
    who.iter().zip(sent).filter_map(|(&i, p)| Some((i, p?))).collect()
}

/// `payloads` sent in member order, each except a lost one to every queue.
fn send(net: &mut Network, plan: &Plan, payloads: Vec<(usize, Vec<u8>)>) {
    for (from, payload) in payloads {
        if plan.lost.contains(&(net.phase, from)) {
            let own: Arc<[u8]> = payload.into();
            net.nodes[from].queue.push_back((Sender::Own, own));
        } else {
            net.send(from, &payload);
        }
    }
}

/// Each member in `receivers` drains its queue in pops of 8; every other queue is dropped.
fn receive(net: &mut Network, receivers: &[usize]) {
    for (i, node) in net.nodes.iter_mut().enumerate() {
        if !receivers.contains(&i) {
            node.queue.clear();
        }
    }
    let phase = net.phase;
    each(net, receivers, |node, bls| node.drain(bls, phase, |_| 8));
}

/// What a run's members committed and its finalizers built.
struct Finals {
    /// The members that sent a premature commitment.
    committed: Vec<usize>,
    /// Each finalizer's final commitments.
    by: Vec<(usize, Vec<LlmqCommitment>)>,
}

/// The run of `plan` on `net`, phase by phase as Core's handler starts them.
fn run(net: &mut Network, plan: &Plan) -> Finals {
    let n = net.nodes.len();
    let online = plan.online(n);
    for &(i, kind, rate) in &plan.sim {
        net.nodes[i].sim.set(kind, rate).expect("a rate");
    }
    let liars: Vec<usize> = plan.liars.iter().map(|(i, _)| *i).collect();
    assert!(liars.iter().all(|i| !plan.lost.contains(&(DkgMsgKind::Contribution, *i))));

    plan.begin(net, DkgMsgKind::Contribution);
    let honest: Vec<usize> = online.iter().copied().filter(|i| !liars.contains(i)).collect();
    let sent = each(net, &honest, |node, bls| {
        node.step(bls, |s, env| s.contribute(env).expect("first call"))
            .map(|qc| qc.payload(bls.scheme()))
    });
    send(net, plan, sent_by(&honest, sent));
    for (i, lie_to) in &plan.liars {
        net.contribute_modified(*i, lie_to);
    }
    receive(net, &online);

    plan.begin(net, DkgMsgKind::Complaint);
    let sent = each(net, &online, |node, bls| {
        node.step(bls, |s, env| s.verify_and_complain(&ConnSnapshot::default(), env))
            .expect("in phase order")
            .map(|qc| qc.payload(bls.scheme()))
    });
    send(net, plan, sent_by(&online, sent));
    receive(net, &online);

    plan.begin(net, DkgMsgKind::Justification);
    let sent = each(net, &online, |node, bls| {
        node.step(bls, |s, env| s.verify_and_justify(env))
            .expect("in phase order")
            .map(|qj| qj.payload(bls.scheme()))
    });
    send(net, plan, sent_by(&online, sent));
    receive(net, &online);

    plan.begin(net, DkgMsgKind::PrematureCommitment);
    let sent = each(net, &online, |node, bls| {
        node.step(bls, |s, env| s.verify_and_commit(env)).map(|qc| qc.payload(bls.scheme()))
    });
    let payloads = sent_by(&online, sent);
    let finalizers = if plan.finalizers.is_empty() {
        online.clone()
    } else {
        plan.finalizers.clone()
    };
    let committed = payloads.iter().map(|(i, _)| *i).collect();
    send(net, plan, payloads);
    receive(net, &finalizers);

    let finals =
        each(net, &finalizers, |node, bls| node.step(bls, |s, env| s.finalize_commitments(env)));
    Finals {
        committed,
        by: finalizers.into_iter().zip(finals).collect(),
    }
}

/// What every final of a run must be.
struct Want {
    version: u16,
    quorum_index: Option<i16>,
    valid: Vec<bool>,
    signers: Vec<bool>,
}

impl Want {
    /// A quorum of `size` at index 0 that does not rotate, V19 active after the base, whose
    /// members are all valid and all sign.
    fn honest(size: usize) -> Self {
        Self {
            version: 3,
            quorum_index: None,
            valid: vec![true; size],
            signers: vec![true; size],
        }
    }

    /// `out` neither valid nor signing.
    fn without(self, out: &[usize]) -> Self {
        self.invalid(out).unsigned(out)
    }

    /// `out` not valid; each still signs its own premature commitment, as Core's
    /// `SendCommitment` lets a member it marked bad.
    fn invalid(mut self, out: &[usize]) -> Self {
        for &i in out {
            self.valid[i] = false;
        }
        self
    }

    /// `out` not signing.
    fn unsigned(mut self, out: &[usize]) -> Self {
        for &i in out {
            self.signers[i] = false;
        }
        self
    }
}

fn bits(set: &dash_primitives::DynBitset) -> Vec<bool> {
    (0..set.num_bits()).map(|i| set.get(i) == Some(true)).collect()
}

/// T4's acceptance: each finalizer built exactly one final, all of them byte-equal; a verifier
/// built here accepts it with the signatures checked, reading the members twice; and it has
/// `want`'s version, quorumIndex, `validMembers` and signers.
fn accept(net: &Network, finals: &Finals, want: &Want) {
    let Some((_, first)) = finals.by.first() else {
        panic!("a finalizer");
    };
    let qc = first.first().expect("a final commitment");
    let bytes = final_commitment_bytes(qc);
    for (i, f) in &finals.by {
        assert_eq!(f.len(), 1, "member {i}: one final commitment");
        assert_eq!(final_commitment_bytes(&f[0]), bytes, "member {i}: the final, byte for byte");
    }
    let mut verifier = net.setup.verifier(net.llmq_type, &net.params, &net.members);
    verifier.bls = net.bls;
    assert!(
        verifier.verify(&net.setup.base.hash, qc),
        "verify_commitment(Sync): {:?}",
        verifier.calls[0].verdict
    );
    assert_eq!(verifier.member_reads(), [2], "the signatures were checked");
    assert_eq!(qc.version(), want.version, "version");
    assert_eq!(qc.quorum_index(), want.quorum_index, "quorumIndex");
    assert_eq!(bits(qc.valid_members()), want.valid, "validMembers");
    assert_eq!(bits(qc.signers()), want.signers, "signers");
}

fn network(t: LlmqType, n: usize) -> Network {
    Network::new(t, &params(t), n, 0x7a4 + n as u64)
}

fn honest_run(t: LlmqType) {
    let n = usize::from(params(t).size);
    let mut net = network(t, n);
    let finals = run(&mut net, &Plan::default());
    assert_eq!(finals.by.len(), n);
    assert_eq!(finals.committed, (0..n).collect::<Vec<_>>());
    accept(&net, &finals, &Want::honest(n));
    for node in &net.nodes {
        assert!(node.misbehaving.is_empty());
    }
}

#[test]
fn honest_llmq_test_3() {
    honest_run(LlmqType::LlmqTest);
}

#[test]
fn honest_llmq_test_dip0024_4() {
    let mut net = dip0024_4();
    let finals = run(&mut net, &Plan::default());
    assert_eq!(finals.committed, [0, 1, 2, 3]);
    accept(&net, &finals, &rotated_want(4, 0));
}

#[test]
fn honest_llmq_devnet_12() {
    honest_run(LlmqType::LlmqDevnet);
}

#[test]
fn honest_llmq_50_60() {
    honest_run(LlmqType::Llmq50_60);
}

#[test]
fn llmq_test_3_with_a_dropout() {
    let mut net = network(LlmqType::LlmqTest, 3);
    let plan = Plan {
        offline: [2].into(),
        ..Plan::default()
    };
    let finals = run(&mut net, &plan);
    accept(&net, &finals, &Want::honest(3).without(&[2]));
}

#[test]
fn llmq_test_3_with_a_justified_contribution_lie_and_a_commit_lie() {
    let mut net = network(LlmqType::LlmqTest, 3);
    let plan = Plan {
        sim: vec![(0, SimError::ContributionLie, 100), (1, SimError::CommitLie, 100)],
        ..Plan::default()
    };
    let finals = run(&mut net, &plan);
    assert_eq!(finals.committed, [0, 1, 2], "the liar sends its lie");
    accept(&net, &finals, &Want::honest(3).unsigned(&[1]));
    for i in [1, 2] {
        assert!(complained(&net.nodes[i], 0), "member {i}");
    }
}

/// Whether `node` set its `weComplain` bit for member `idx` (`UpdateLocalMemberStatus`).
fn complained(node: &Node, idx: usize) -> bool {
    node.debug.updates.contains(&DebugUpdate::Member(idx, MemberStatusBit::WeComplain))
}

fn count(node: &Node, text: &str) -> usize {
    node.events.iter().filter(|e| e.text == text).count()
}

/// `llmq_test_dip0024`'s 4 members at quorum index 0, rotated as the type always is.
fn dip0024_4() -> Network {
    let t = LlmqType::LlmqTestDip0024;
    let p = params(t);
    Network::with_setup(t, &p, 4, 0x7a8, NetworkSetup::rotated(&p, 0))
}

/// What a rotated quorum's final must be at `index`.
fn rotated_want(n: usize, index: i16) -> Want {
    Want {
        version: 4,
        quorum_index: Some(index),
        ..Want::honest(n)
    }
}

#[test]
fn llmq_test_dip0024_4_with_a_justified_lie() {
    let mut net = dip0024_4();
    let plan = Plan {
        liars: vec![(1, [0, 3].into())],
        ..Plan::default()
    };
    let finals = run(&mut net, &plan);
    accept(&net, &finals, &rotated_want(4, 0));
    for i in [0, 3] {
        assert!(complained(&net.nodes[i], 1), "member {i}");
    }
}

/// With minSize = size, a dropout leaves no quorum, and so does a lost complaint: the liar
/// justifies only the complaint it received, so the member whose complaint was lost marks it
/// bad, is left below minSize and commits nothing, and the other three are too few to finalize.
#[test]
fn llmq_test_dip0024_4_with_a_lost_complaint_finalizes_nothing() {
    let mut net = dip0024_4();
    let plan = Plan {
        liars: vec![(1, [0, 3].into())],
        lost: vec![(DkgMsgKind::Complaint, 0)],
        ..Plan::default()
    };
    let finals = run(&mut net, &plan);
    assert_eq!(finals.committed, [1, 2, 3]);
    assert_eq!(count(&net.nodes[0], "not enough valid members. not sending commitment"), 1);
    for (i, node) in net.nodes.iter().enumerate() {
        assert_eq!(node.session.is_bad(1), i == 0, "member {i}");
    }
    assert!(finals.by.iter().all(|(_, f)| f.is_empty()), "no final commitment");
}

#[test]
fn llmq_devnet_12_with_dropouts_and_an_unjustified_lie() {
    let mut net = network(LlmqType::LlmqDevnet, 12);
    let plan = Plan {
        offline: [4, 9].into(),
        liars: vec![(3, [2, 5].into())],
        lost: vec![(DkgMsgKind::Justification, 3)],
        ..Plan::default()
    };
    let finals = run(&mut net, &plan);
    // The liar's own justification answers the complaints in its own view only: its premature
    // commitment's `validMembers` differ from everyone else's, so its group is below minSize.
    accept(&net, &finals, &Want::honest(12).without(&[3, 4, 9]));
    assert!(!net.nodes[3].session.is_bad(3));
}

#[test]
fn llmq_devnet_12_with_five_dropouts_leaves_exactly_min_size() {
    let p = params(LlmqType::LlmqDevnet);
    assert_eq!(p.size - 5, p.min_size);
    let mut net = network(LlmqType::LlmqDevnet, 12);
    let out = [1, 4, 6, 9, 11];
    let plan = Plan {
        offline: out.into(),
        ..Plan::default()
    };
    let finals = run(&mut net, &plan);
    assert_eq!(finals.by.len(), 7);
    accept(&net, &finals, &Want::honest(12).without(&out));
}

#[test]
fn llmq_50_60_with_dropouts_and_lies() {
    let mut net = network(LlmqType::Llmq50_60, 50);
    let offline: BTreeSet<usize> = (40..50).step_by(2).collect();
    let plan = Plan {
        offline: offline.clone(),
        liars: vec![(7, [1, 20, 33].into()), (12, [0, 44].into())],
        lost: vec![(DkgMsgKind::Justification, 12)],
        ..Plan::default()
    };
    let finals = run(&mut net, &plan);
    let mut out: Vec<usize> = offline.into_iter().collect();
    out.push(12);
    accept(&net, &finals, &Want::honest(50).without(&out));
}

#[test]
fn llmq_100_67_with_several_mid_phase_triggers() {
    // Three verifies inside a pop, then a remainder of 4 when the Complain phase starts.
    let (net, _) = sampled(LlmqType::Llmq100_67);
    for node in &net.nodes {
        assert_eq!(count(node, "verified 32 pending contributions"), 3);
        assert_eq!(count(node, "verified 4 pending contributions"), 1);
    }
}

/// A quorum whose base is before V19 (LQ4 §4.3, m11), at `bls_at`'s schemes.
fn pre_v19(bls_at: Vec<(DkgMsgKind, BlsCtx)>) -> (Network, Finals) {
    let t = LlmqType::LlmqTest;
    let p = params(t);
    let setup = NetworkSetup {
        v19_after_base: false,
        ..NetworkSetup::new(&p)
    };
    let mut net = Network::with_setup(t, &p, 3, 0x519, setup);
    let plan = Plan {
        bls_at,
        ..Plan::default()
    };
    let finals = run(&mut net, &plan);
    (net, finals)
}

#[test]
fn a_pre_v19_quorum_finalizes_at_version_1() {
    let (net, finals) = pre_v19(vec![(DkgMsgKind::Contribution, BlsCtx::LEGACY)]);
    let want = Want {
        version: 1,
        ..Want::honest(3)
    };
    accept(&net, &finals, &want);
}

/// The flag straddle (LQ4 §4.3, m11): the base is before V19 and the tip crosses it once the
/// contributions are in, so the sessions sign under the legacy scheme (`m_use_legacy_bls`,
/// v24.0.0 `a/dkgsession.cpp:35`) while the batch signature check runs under the process-wide
/// basic one (`l/net_dkg.cpp:296`). Every premature commitment then fails it: each member
/// scores each other once, its own batch fails too, and nobody finalizes.
#[test]
fn flag_straddle_pre_v19_base_post_v19_tip_finalizes_nothing() {
    let (net, finals) = pre_v19(vec![
        (DkgMsgKind::Contribution, BlsCtx::LEGACY),
        (DkgMsgKind::Complaint, BlsCtx::BASIC),
    ]);
    assert_eq!(finals.committed, [0, 1, 2], "each member sends its premature commitment");
    for (i, node) in net.nodes.iter().enumerate() {
        let others: Vec<Misbehaviour<usize>> = (0..3)
            .filter(|&j| j != i)
            .map(|peer| Misbehaviour::BadSignature {
                peer,
            })
            .collect();
        assert_eq!(node.misbehaving, others, "member {i}");
        assert_eq!(count(node, "failed to verify signature, peer=-1"), 1, "member {i}");
        assert!(node.verify.calls.is_empty(), "member {i}: no final to verify");
    }
    assert!(finals.by.iter().all(|(_, f)| f.is_empty()), "no final commitment");
}

fn rotated(t: LlmqType, index: i32) {
    let p = params(t);
    let n = usize::from(p.size);
    let mut net = Network::with_setup(t, &p, n, 0x40 + n as u64, NetworkSetup::rotated(&p, index));
    let finals = run(&mut net, &Plan::default());
    accept(&net, &finals, &rotated_want(n, i16::try_from(index).expect("small")));
}

#[test]
fn rotated_llmq_test_dip0024_at_index_1() {
    rotated(LlmqType::LlmqTestDip0024, 1);
}

#[test]
fn rotated_llmq_60_75_at_index_1() {
    rotated(LlmqType::Llmq60_75, 1);
}

/// `contribution-invalid` (DEC-1343): member 0 sends each other member a share that decrypts
/// to the invalid (zero) secret key. Each recipient complains through the rc.5 intake, and
/// member 0's justification reveals its true shares, so it stays valid.
#[test]
fn contribution_invalid_is_complained_about_and_justified() {
    let mut net = network(LlmqType::LlmqTest, 3);
    let plan = Plan {
        sim: vec![(0, SimError::ContributionInvalid, 100)],
        ..Plan::default()
    };
    let finals = run(&mut net, &plan);
    accept(&net, &finals, &Want::honest(3));
    let pro_tx = |i: usize| net.members[i].pro_tx_hash;
    for i in [1, 2] {
        let line = format!("contribution from {} decrypted to an invalid share", pro_tx(0));
        assert_eq!(count(&net.nodes[i], &line), 1, "member {i}");
        assert!(complained(&net.nodes[i], 0), "member {i}");
        let sent = format!("sending invalid (zero) share to {}", pro_tx(i));
        assert_eq!(count(&net.nodes[0], &sent), 1, "member {i}");
    }
    assert!(!complained(&net.nodes[0], 0), "no share to itself is drawn for");
}

/// `contribution-invalid` with `justify-omit`: the complaints stay open, so every member, the
/// sender too, marks it bad when commit starts and leaves it out of `validMembers`; it still
/// signs its own premature commitment, which agrees with the others'.
#[test]
fn contribution_invalid_left_unjustified_leaves_the_sender_out() {
    let mut net = network(LlmqType::LlmqTest, 3);
    let plan = Plan {
        sim: vec![(0, SimError::ContributionInvalid, 100), (0, SimError::JustifyOmit, 100)],
        ..Plan::default()
    };
    let finals = run(&mut net, &plan);
    accept(&net, &finals, &Want::honest(3).invalid(&[0]));
    for (i, node) in net.nodes.iter().enumerate() {
        assert!(node.session.is_bad(0), "member {i}");
    }
}

/// 8 members spread over a quorum of `n`.
fn sample(n: usize) -> Vec<usize> {
    (0..8).map(|k| n / 8 * k + 7).collect()
}

/// An honest quorum of `t`'s size in which every member commits, and the premature
/// commitments' checks and finalization run on [`sample`]'s members (DEC-616 #5). The time the
/// run took.
fn sampled(t: LlmqType) -> (Network, std::time::Duration) {
    let n = usize::from(params(t).size);
    let mut net = network(t, n);
    let sample = sample(n);
    let plan = Plan {
        finalizers: sample.clone(),
        ..Plan::default()
    };
    let start = Instant::now();
    let finals = run(&mut net, &plan);
    let took = start.elapsed();
    assert_eq!(finals.committed, (0..n).collect::<Vec<_>>(), "every member committed");
    assert_eq!(finals.by.iter().map(|(i, _)| *i).collect::<Vec<_>>(), sample);
    accept(&net, &finals, &Want::honest(n));
    for i in sample {
        let relayed = &net.nodes[i].relayed;
        assert_eq!(relayed.len(), 2 * n, "member {i}: each contribution and premature commitment");
    }
    (net, took)
}

fn four_hundred(t: LlmqType) {
    assert_eq!(params(t).size, 400);
    let (_, took) = sampled(t);
    eprintln!("{t:?}: 400 members on at most {THREADS} threads in {took:.1?}");
}

#[test]
#[ignore = "nightly: about 2 h wall and 7 CPU-h on 4 threads, above tier 1's 30 minutes (DEC-616 #5)"]
fn llmq_400_60_every_member_commits() {
    four_hundred(LlmqType::Llmq400_60);
}

#[test]
#[ignore = "nightly: about 2.5 h wall and 10 CPU-h on 4 threads, above tier 1's 30 minutes (DEC-616 #5)"]
fn llmq_400_85_every_member_commits() {
    four_hundred(LlmqType::Llmq400_85);
}

const ROTATION: (&str, &str) =
    ("dkg_lq4b_rotation.json5", "6815a7b4c19deba0254de0bf1d0444087eecc8364c9e94a5bd7b926f1f3a9ae6");

fn unhex(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex"))
        .collect()
}

fn hexes(value: &Value) -> Vec<u8> {
    unhex(value.as_str().expect("a hex string"))
}

fn index(value: &Value) -> usize {
    usize::try_from(value.as_u64().expect("an index")).expect("small")
}

/// The C0 file, once its sha256 is the one `PROVENANCE.md` names, without its `//` lines.
fn corpus((name, sha): (&str, &str)) -> Value {
    let path = format!("{}/../test-vectors/lq4/{name}", env!("CARGO_MANIFEST_DIR"));
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let hex: String = sha256(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(hex, sha, "{name} is not the copy PROVENANCE.md names");
    let text = std::str::from_utf8(&bytes).expect("UTF-8");
    let json: Vec<&str> = text.lines().filter(|l| !l.trim_start().starts_with("//")).collect();
    serde_json::from_str(&json.join("\n")).expect("JSON without the comment lines")
}

/// The effective parameters of `t` in a C0 file.
fn effective(file: &Value, t: LlmqType) -> LlmqParams {
    let p = &file["params"][t.to_base().to_string()];
    let int = |k: &str| u16::try_from(p[k].as_u64().expect("an int")).expect("small");
    LlmqParams {
        size: int("size"),
        min_size: int("min_size"),
        threshold: int("threshold"),
        dkg_bad_votes_threshold: int("bad_votes"),
        ..params(t)
    }
}

/// A captured member: its proTxHash and its operator key.
fn member(row: &Value) -> (ProTxHash, TestOperatorKey) {
    let pro_tx: [u8; 32] = hexes(&row[0]).try_into().expect("32 bytes");
    let sk: [u8; 32] = hexes(&row[1]).try_into().expect("32 bytes");
    let key = BlsSkValue::from_canonical(&sk, BlsSchemeKind::Basic).expect("a secret key");
    (ProTxHash::from(pro_tx), TestOperatorKey::new(key))
}

/// The `[member, raw]` rows of `rows` into `session` in pops of 8, each from its member as a
/// peer: nobody is scored, and each message is relayed once, as its own kind. The count fed.
fn feed(
    session: &mut DkgSession<TestMember>,
    kind: DkgMsgKind,
    rows: &Value,
    env: &mut DkgEnv<'_>,
    label: &str,
) -> usize {
    let msgs: Vec<(usize, Vec<u8>)> =
        rows.as_array().expect("rows").iter().map(|r| (index(&r[0]), hexes(&r[1]))).collect();
    for pop in msgs.chunks(8) {
        let batch: Vec<(Sender<usize>, &[u8])> =
            pop.iter().map(|(from, raw)| (Sender::Peer(*from), &raw[..])).collect();
        let out = session.process_batch(kind, &batch, env);
        assert!(out.misbehaving.is_empty(), "{label} {kind:?}");
        let relayed: Vec<DkgInv> = out.relay.iter().map(|r| r.inv).collect();
        let want: Vec<DkgInv> = pop
            .iter()
            .map(|(_, raw)| DkgInv {
                kind,
                hash: sha256d(raw),
            })
            .collect();
        assert_eq!(relayed, want, "{label} {kind:?}: each relayed once");
    }
    msgs.len()
}

/// Non-member replay (LQ4 §4.3, m2): each session of the C0 rotation capture, replayed from a
/// masternode of the same run outside the quorum. Its session has no relay members and no
/// local session status; it relays every captured message and scores nobody; it writes no
/// dkgdb row, sends nothing and finalizes nothing; its verdicts are those of Core's final, which
/// the verifier accepts at the session's base with its version and quorumIndex.
#[test]
fn a_non_member_replays_every_rotation_capture_session() {
    let file = corpus(ROTATION);
    assert_eq!(file["scheme"], "basic");
    for tag in ["rc4", "rc5"] {
        let sessions = file[tag].as_array().expect("sessions");
        let everyone: BTreeMap<ProTxHash, TestOperatorKey> = sessions
            .iter()
            .flat_map(|q| q["members"].as_array().expect("members").iter().map(member))
            .collect();
        let mut seen = Vec::new();
        for q in sessions {
            let t = LlmqType::from_base(u8::try_from(index(&q["llmq_type"])).expect("a type"));
            let p = effective(&file, t);
            let label = format!("{tag} {t:?} {}", &q["quorum_hash"].as_str().expect("hex")[..8]);
            let (pro_txs, keys): (Vec<ProTxHash>, Vec<TestOperatorKey>) =
                q["members"].as_array().expect("members").iter().map(member).unzip();
            let members: Vec<TestMember> = pro_txs
                .iter()
                .zip(&keys)
                .map(|(pro_tx, key)| TestMember::new(*pro_tx, key.public_key()))
                .collect();
            let (outsider, key) =
                everyone.iter().find(|(m, _)| !pro_txs.contains(m)).expect("an outsider");
            let outsider = *outsider;

            let raw = hexes(&q["final"]);
            let qc = LlmqCommitment::decode(&mut &raw[..]).expect("a final commitment");
            assert_eq!(final_commitment_bytes(&qc), raw, "{label}");
            let quorum_index = qc.quorum_index().unwrap_or(0);
            let mut setup = NetworkSetup {
                rotation_enabled: qc.quorum_index().is_some(),
                ..NetworkSetup::rotated(&p, i32::from(quorum_index))
            };
            setup.base.hash = QuorumHash::from(*qc.quorum_hash().as_bytes());
            seen.push((t.to_base(), qc.version(), quorum_index));

            let mut debug = RecordingDebug::default();
            let mut topo = CoreRelayTopology {
                members: pro_txs.clone(),
                calls: 0,
            };
            let init = SessionInit {
                llmq_type: t,
                params: &p,
                base: setup.base,
                quorum_index: setup.quorum_index,
                members: Arc::from(members.clone()),
                active_pro_tx: Some(outsider),
                rotation_enabled: setup.rotation_enabled,
                v19_after_base: setup.v19_after_base,
            };
            let mut session: DkgSession<TestMember> =
                DkgSession::init(init, &mut topo, &mut debug).expect("enough members");
            assert!(!session.are_we_member(), "{label}");
            assert_eq!(topo.calls, 0, "{label}: no relay members asked for");
            assert!(session.relay_members().is_empty(), "{label}");

            let mut store = MemStore::with_members(pro_txs.clone());
            let mut rng = CoreRng(SeededRng::new(0x0b5));
            let sim = SimErrorRates::default();
            let mut verify = setup.verifier(t, &p, &members);
            let mut env = DkgEnv {
                bls: BlsCtx::BASIC,
                rng: &mut rng,
                key,
                store: &mut store,
                debug: &mut debug,
                sim: &sim,
                runner: &SerialRunner,
                verify: &mut verify,
            };
            assert!(session.contribute(&mut env).expect("phase order").is_none(), "{label}");
            feed(&mut session, DkgMsgKind::Contribution, &q["contributions"], &mut env, &label);
            let conn = ConnSnapshot::default();
            assert!(session.verify_and_complain(&conn, &mut env).expect("phase order").is_none());
            feed(&mut session, DkgMsgKind::Complaint, &q["complaints"], &mut env, &label);
            assert!(session.verify_and_justify(&mut env).expect("phase order").is_none());
            feed(&mut session, DkgMsgKind::Justification, &q["justifications"], &mut env, &label);
            assert!(session.verify_and_commit(&mut env).is_none(), "{label}");
            let pcommits = feed(
                &mut session,
                DkgMsgKind::PrematureCommitment,
                &q["premature"],
                &mut env,
                &label,
            );
            assert_eq!(pcommits, 4, "{label}");
            assert!(session.finalize_commitments(&mut env).is_empty(), "{label}");

            assert!(store.rows.is_empty(), "{label}: no dkgdb row");
            assert_eq!(store.member_accesses(), pcommits, "{label}: one per premature commitment");
            assert!(
                !debug.updates.iter().any(|u| matches!(u, DebugUpdate::InitSession { .. })),
                "{label}: no local session status"
            );
            assert!(verify.calls.is_empty(), "{label}");
            let verdicts: Vec<bool> = (0..members.len()).map(|j| !session.is_bad(j)).collect();
            assert_eq!(verdicts, bits(qc.valid_members()), "{label}: Core's verdicts");

            let mut check = setup.verifier(t, &p, &members);
            assert!(check.verify(&setup.base.hash, &qc), "{label}: {:?}", check.calls[0].verdict);
            assert_eq!(check.member_reads(), [2], "{label}");
        }
        seen.sort_unstable();
        let want: Vec<(u8, u16, i16)> =
            [(100, 3, 0); 5].into_iter().chain([(103, 4, 0); 3]).chain([(103, 4, 1); 3]).collect();
        assert_eq!(seen, want, "{tag}: rotated sessions at index 0 and 1");
    }
}
