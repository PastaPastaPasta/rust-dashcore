//! The served sink (DK1-L1, owner ruling Q1): what it was told answers each `Get*` as the session
//! does (`l/dkgsession.cpp:660-694`, v24.0.0), after every start function and every batch.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};

use proptest::prelude::*;

use super::fixture::BLS;
use super::*;
use crate::dkg::conn::ConnSnapshot;
use crate::dkg::reference::{Network, seeded_rng};
use crate::dkg::rng::SeededRng;

/// A sink that keeps what it is told as Core's maps keep it.
#[derive(Default)]
pub(crate) struct RecordingSink(Mutex<Recorded>);

#[derive(Default)]
struct Recorded {
    contributions: BTreeMap<[u8; 32], Arc<Contribution>>,
    complaints: BTreeMap<[u8; 32], Arc<Complaint>>,
    justifications: BTreeMap<[u8; 32], Arc<Justification>>,
    premature_commitments: BTreeMap<[u8; 32], Arc<PrematureCommitment>>,
    valid: BTreeSet<[u8; 32]>,
    calls: usize,
}

impl ServedSink for RecordingSink {
    fn contribution(&self, hash: &[u8; 32], qc: &Arc<Contribution>) {
        self.call().contributions.entry(*hash).or_insert_with(|| qc.clone());
    }
    fn complaint(&self, hash: &[u8; 32], qc: &Arc<Complaint>) {
        self.call().complaints.entry(*hash).or_insert_with(|| qc.clone());
    }
    fn justification(&self, hash: &[u8; 32], qj: &Arc<Justification>) {
        self.call().justifications.entry(*hash).or_insert_with(|| qj.clone());
    }
    fn premature_commitment(&self, hash: &[u8; 32], qc: &Arc<PrematureCommitment>) {
        self.call().premature_commitments.entry(*hash).or_insert_with(|| qc.clone());
    }
    fn valid_commitment(&self, hash: &[u8; 32]) {
        self.call().valid.insert(*hash);
    }
}

impl RecordingSink {
    fn lock(&self) -> MutexGuard<'_, Recorded> {
        self.0.lock().expect("not poisoned")
    }

    fn call(&self) -> MutexGuard<'_, Recorded> {
        let mut r = self.lock();
        r.calls += 1;
        r
    }

    /// How many times the session called the sink.
    pub(crate) fn calls(&self) -> usize {
        self.lock().calls
    }

    /// The sink answers every lookup as `session` does, for each hash either has recorded.
    pub(crate) fn assert_serves_as<E: OperatorKeyView>(&self, session: &DkgSession<E>) {
        let flag = BLS.scheme();
        self.assert_serves_commitments_as(session);
        let r = self.lock();
        assert!(r.contributions.keys().eq(session.contributions.keys()));
        for (hash, qc) in &r.contributions {
            let got = session.contribution(hash).map(|m| m.payload(flag));
            assert_eq!(got, Some(qc.payload(flag)));
        }
        assert!(r.complaints.keys().eq(session.complaints.keys()));
        for (hash, qc) in &r.complaints {
            let got = session.complaint(hash).map(|m| m.payload(flag));
            assert_eq!(got, Some(qc.payload(flag)));
        }
        assert!(r.justifications.keys().eq(session.justifications.keys()));
        for (hash, qj) in &r.justifications {
            let got = session.justification(hash).map(|m| m.payload(flag));
            assert_eq!(got, Some(qj.payload(flag)));
        }
    }

    /// [`Self::assert_serves_as`] for the premature commitments alone.
    pub(crate) fn assert_serves_commitments_as<E: OperatorKeyView>(&self, session: &DkgSession<E>) {
        let flag = BLS.scheme();
        let r = self.lock();
        assert!(r.premature_commitments.keys().eq(session.premature_commitments.keys()));
        assert_eq!(r.valid, session.valid_commitments);
        for (hash, qc) in &r.premature_commitments {
            let served = r.valid.contains(hash).then(|| qc.payload(flag));
            let got = session.get_premature_commitment(hash).map(|m| m.payload(flag));
            assert_eq!(got, served);
        }
    }
}

/// Every member's session with its own sink.
fn attach(net: &mut Network) -> Vec<Arc<RecordingSink>> {
    net.nodes
        .iter_mut()
        .map(|node| {
            let sink = Arc::new(RecordingSink::default());
            node.session = node.session.clone().with_served(sink.clone());
            sink
        })
        .collect()
}

fn check(net: &Network, sinks: &[Arc<RecordingSink>]) {
    for (node, sink) in net.nodes.iter().zip(sinks) {
        sink.assert_serves_as(&node.session);
    }
}

/// Every member pops `pop` at a time until its queue is empty, checked after each batch.
fn drain_checked(net: &mut Network, sinks: &[Arc<RecordingSink>], pop: usize) {
    let (bls, phase) = (net.bls, net.phase);
    for (node, sink) in net.nodes.iter_mut().zip(sinks) {
        while node.pop(bls, phase, pop) {
            sink.assert_serves_as(&node.session);
        }
    }
}

/// A whole `llmq_test` round of `n` members: `liar` lies to `lied_to` in its contribution, and
/// a member in `silent` never sends its justification.
fn round(
    seed: u64,
    n: usize,
    liar: usize,
    lied_to: &BTreeSet<usize>,
    silent: bool,
    pop: usize,
) -> (Network, Vec<Arc<RecordingSink>>) {
    let params = fixture::params(LlmqType::LlmqTest);
    let weights = || -> Box<dyn CryptoRng + Send> { Box::new(SeededRng::new(0x3e1)) };
    let mut net = Network::new(LlmqType::LlmqTest, &params, n, seed, weights, seeded_rng);
    let sinks = attach(&mut net);
    check(&net, &sinks);

    for i in 0..n {
        if i == liar && !lied_to.is_empty() {
            net.contribute_modified(i, lied_to);
        } else {
            net.contribute(i);
        }
        check(&net, &sinks);
    }
    drain_checked(&mut net, &sinks, pop);

    net.enter(DkgMsgKind::Complaint);
    for i in 0..n {
        net.complain(i, &ConnSnapshot::default());
        check(&net, &sinks);
    }
    drain_checked(&mut net, &sinks, pop);

    net.enter(DkgMsgKind::Justification);
    for i in 0..n {
        net.justify(i, !(silent && i == liar));
        check(&net, &sinks);
    }
    drain_checked(&mut net, &sinks, pop);

    net.enter(DkgMsgKind::PrematureCommitment);
    for i in 0..n {
        let bls = net.bls;
        let qc = net.nodes[i].step(bls, |s, env| s.verify_and_commit(env));
        if let Some(qc) = qc {
            net.send(i, &qc.payload(bls.scheme()));
        }
        check(&net, &sinks);
    }
    drain_checked(&mut net, &sinks, pop);
    (net, sinks)
}

#[test]
fn a_lie_answered_by_a_justification_reaches_every_sink() {
    // Mutants: any one of the five sink calls dropped.
    let (net, sinks) = round(0x5e7, 3, 0, &BTreeSet::from([1]), false, 8);
    for (node, sink) in net.nodes.iter().zip(&sinks) {
        let s = &node.session;
        assert_eq!(s.contributions.len(), 3);
        assert_eq!((s.complaints.len(), s.justifications.len()), (1, 1));
        assert_eq!((s.premature_commitments.len(), s.valid_commitments.len()), (3, 3));
        assert_eq!(sink.calls(), 3 + 1 + 1 + 3 + 3);
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 6, ..ProptestConfig::default() })]

    #[test]
    fn the_sink_serves_as_the_session_after_every_call(
        seed in any::<u64>(),
        n in 2usize..=3,
        liar in 0usize..3,
        lied_to in proptest::collection::btree_set(0usize..3, 0..=2),
        silent in any::<bool>(),
        pop in 1usize..=8,
    ) {
        let liar = liar % n;
        let lied_to: BTreeSet<usize> = lied_to.into_iter().filter(|&j| j < n && j != liar).collect();
        round(seed, n, liar, &lied_to, silent, pop);
    }
}
