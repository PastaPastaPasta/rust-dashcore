//! The node's block processor handler ([`NetQuorumBlockProcessor`]) in its registry, driven in the
//! processor's sim over a stand-in chain: the solicitation gate, each verdict's score and relay,
//! `AlreadyHave` and `getdata`.
// Core: v24.0.0 src/llmq/blockprocessor.cpp:98-211; src/net_processing.cpp:2438-2439, :3029-3035,
// :5802

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dash_consensus::primitives::{P2pCommitment, commitment_hash};
use dash_dml::blockprocessor::QcBlockError;
use dash_dml::blockprocessor::pool::{QfcommitOutcome, null_commitment};
use dash_dml::store::{NodeMemberError, StoreError};
use dash_evo::llmq::{MemberError, ParamsOutOfDomain};
use dash_num::Hash256;
use dash_p2p::{CommandString, ConnectionType, Permissions};
use dash_p2p_core::{GetData, Inv, InvType, Inventory, NotFound};
use dash_params::{ChainParamsBuilder, LlmqType, Network, Release};
use dash_primitives::{Commitment, QuorumHash};
use dash_types::codec::BaseCodec;
use dash_types::{Hashable as _, Numeric as _};
use dashd_chainstate::chain::OnVMembersError;
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::seams::NoPlatformBans;
use dashd_p2p::processing::serve_mnlistdiff::NoEvoServe;
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, Sim, SimPeer};
use dashd_p2p::{ProcessorOptions, Seams};

use super::qfcommit::{Answer, AnswerWait, NetQuorumBlockProcessor, QfcommitChain, answer};
use super::*;

/// The chain's side: one configured answer, the calls it got, and a pool by hash.
struct Fake {
    answer: Mutex<Answer>,
    calls: Mutex<Vec<(Commitment, i32)>>,
    pool: Mutex<BTreeMap<[u8; 32], Arc<Commitment>>>,
}

impl Fake {
    fn new(answer: Answer) -> Arc<Fake> {
        Arc::new(Fake {
            answer: Mutex::new(answer),
            calls: Mutex::default(),
            pool: Mutex::default(),
        })
    }
}

impl QfcommitChain for Fake {
    fn process(&self, qc: P2pCommitment, common_version: i32) -> AnswerWait {
        dashd_p2p::processing::peer::assert_no_header_cell();
        self.calls.lock().unwrap().push((qc.into_commitment(), common_version));
        Box::pin(std::future::ready(self.answer.lock().unwrap().clone()))
    }

    fn has(&self, hash: &[u8; 32]) -> bool {
        self.pool.lock().unwrap().contains_key(hash)
    }

    fn get(&self, hash: &[u8; 32]) -> Option<Arc<Commitment>> {
        self.pool.lock().unwrap().get(hash).cloned()
    }
}

/// Out of IBD, with the node's registry plus the handler over `fake`; an outbound announcer `a`
/// and an inbound peer `b`.
fn sim(fake: &Arc<Fake>) -> (Sim, SimPeer, SimPeer) {
    let mut registry = node_registry(Box::new(NoEvoServe), Arc::new(NoPlatformBans), None);
    registry.register(Box::new(NetQuorumBlockProcessor(Arc::clone(fake) as _)));
    let p = Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let id = local_identity(&p, &IdentityOptions::new()).unwrap();
    let mut sim = Sim::with(p, id, ProcessorOptions::default(), registry, Seams::default(), |_| {});
    sim.chain.set_ibd(false);
    let a = peer(&mut sim, 1, ConnectionType::OutboundFullRelay);
    let b = peer(&mut sim, 2, ConnectionType::Inbound);
    (sim, a, b)
}

fn peer(sim: &mut Sim, n: u8, conn_type: ConnectionType) -> SimPeer {
    let p = sim.connect(n, conn_type, Permissions::NONE);
    sim.handshake(&p, PeerVersionMsg::v24(SIM_START));
    settle(sim);
    sim.sent(&p);
    p
}

fn settle(sim: &mut Sim) {
    while sim.pass() {}
    sim.pass();
}

fn encode<T: BaseCodec>(m: &T) -> Vec<u8> {
    let mut out = Vec::new();
    m.encode(&mut out);
    out
}

/// A non-null-shaped LLMQ_TEST commitment (the stand-in judges it), its wire form and its
/// inventory under Core's `::SerializeHash(qc)`.
fn commitment() -> (Commitment, Vec<u8>, Inventory) {
    let p = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap();
    let t = LlmqType::LlmqTest;
    let qc = null_commitment(t, p.consensus.llmq(t).unwrap(), QuorumHash::from([5; 32]), 1, 0);
    let inv = Inventory::new(InvType::QuorumFinalCommitment, qc.hash());
    (qc.clone(), encode(&qc), inv)
}

fn announce(sim: &mut Sim, p: &SimPeer, invs: &[Inventory]) {
    sim.deliver(p, &[("inv", encode(&Inv::new(invs.to_vec())))]);
    settle(sim);
}

/// The inventories of `cmd` messages `p` was sent since the last read.
fn got(sim: &Sim, p: &SimPeer, cmd: CommandString) -> Vec<Inventory> {
    sim.sent(p)
        .into_iter()
        .filter(|(c, _)| *c == cmd)
        .flat_map(|(c, payload)| match c {
            CommandString::GETDATA => {
                GetData::decode(&mut &payload[..]).unwrap().inventory().clone()
            }
            CommandString::NOTFOUND => {
                NotFound::decode(&mut &payload[..]).unwrap().inventory().clone()
            }
            _ => Inv::decode(&mut &payload[..]).unwrap().inventory().clone(),
        })
        .collect()
}

fn score(sim: &Sim, p: &SimPeer) -> i32 {
    sim.shared.misbehavior(p.handle.id()).unwrap().0
}

/// `a` announces the commitment, is asked for it, and sends it: what `b` was then relayed.
fn requested(fake: &Arc<Fake>) -> (Sim, SimPeer, Vec<Inventory>) {
    let (mut sim, a, b) = sim(fake);
    let (_, payload, inv) = commitment();
    announce(&mut sim, &a, &[inv]);
    assert_eq!(got(&sim, &a, CommandString::GETDATA), [inv]);
    sim.deliver(&a, &[("qfcommit", payload)]);
    settle(&mut sim);
    let relayed = got(&sim, &b, CommandString::INV);
    (sim, a, relayed)
}

/// An accepted commitment the pool took is relayed under its own hash, unscored; the chain got
/// it with the sender's common version.
#[test]
fn an_accepted_qfcommit_is_relayed() {
    let (qc, _, inv) = commitment();
    let fake = Fake::new(Answer::Verdict(QfcommitOutcome::Accepted(Some(commitment_hash(&qc)))));
    let (sim, a, relayed) = requested(&fake);
    assert_eq!(score(&sim, &a), 0);
    assert_eq!(relayed, [inv]);
    let calls = fake.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, qc);
    assert_eq!(calls[0].1, a.handle.common_version().get());
}

/// An accepted commitment the pool did not take (no more signers) is not relayed.
#[test]
fn an_accepted_qfcommit_the_pool_kept_out_is_not_relayed() {
    let fake = Fake::new(Answer::Verdict(QfcommitOutcome::Accepted(None)));
    let (sim, a, relayed) = requested(&fake);
    assert_eq!((score(&sim, &a), relayed), (0, vec![]));
}

/// A punished commitment scores its sender and is not relayed; an ignored one does neither.
#[test]
fn a_verdicts_score_reaches_the_sender() {
    for (answer, want) in [(QfcommitOutcome::Misbehaving(100), 100), (QfcommitOutcome::Ignored, 0)]
    {
        let fake = Fake::new(Answer::Verdict(answer.clone()));
        let (sim, a, relayed) = requested(&fake);
        assert_eq!((score(&sim, &a), relayed), (want, vec![]), "{answer:?}");
    }
}

/// A failed read is Core's caught `dbwrapper_error`, and no verdict and no V answer are drops:
/// each leaves the sender unscored and relays nothing (DEC-1881).
#[test]
fn a_failed_read_or_no_verdict_is_unscored() {
    for answer in [
        Answer::NoVerdict("BLS".to_owned()),
        Answer::ReadFailed("the list is withheld".to_owned()),
        Answer::NoAnswer,
    ] {
        let fake = Fake::new(answer.clone());
        let (sim, a, relayed) = requested(&fake);
        assert_eq!((score(&sim, &a), relayed), (0, vec![]), "{answer:?}");
        assert_eq!(fake.calls.lock().unwrap().len(), 1);
    }
}

/// An unrequested commitment fails the solicitation gate before the chain sees it.
// Core: v24.0.0 src/llmq/blockprocessor.cpp:113-119
#[test]
fn an_unrequested_qfcommit_never_reaches_the_chain() {
    let fake = Fake::new(Answer::Verdict(QfcommitOutcome::Ignored));
    let (mut sim, a, _) = sim(&fake);
    let (_, payload, _) = commitment();
    sim.deliver(&a, &[("qfcommit", payload)]);
    settle(&mut sim);
    assert_eq!(score(&sim, &a), 10);
    assert!(fake.calls.lock().unwrap().is_empty());
}

/// A commitment that arrives after the tracker gave up on the request, but within the record's
/// window, is a late answer: judged like a requested one, its sender unscored.
// Core: v24.0.0 src/net_processing.cpp:6925-6957 PeerConsumeGetDataResponse
#[test]
fn a_late_qfcommit_reaches_the_chain() {
    let fake = Fake::new(Answer::Verdict(QfcommitOutcome::Ignored));
    let (mut sim, a, _) = sim(&fake);
    let (_, payload, inv) = commitment();
    announce(&mut sim, &a, &[inv]);
    assert_eq!(got(&sim, &a, CommandString::GETDATA), [inv]);
    let requests = sim.lp.logic().requested_objects();
    assert_eq!(requests.count(a.handle.id()), 1);
    sim.clock.advance(Duration::from_secs(90));
    settle(&mut sim);
    assert_eq!(requests.count(a.handle.id()), 0, "the tracker gave up on the request");
    sim.deliver(&a, &[("qfcommit", payload)]);
    settle(&mut sim);
    assert_eq!(score(&sim, &a), 0);
    assert_eq!(fake.calls.lock().unwrap().len(), 1);
}

/// Route B's result as the handler applies it: a store fault and a failed read are failed reads
/// (unscored), any other refusal is no verdict.
#[test]
fn route_bs_result_maps_to_its_answer() {
    let read = || StoreError::read("the list is withheld");
    assert_eq!(
        answer(Ok(Ok(QfcommitOutcome::Misbehaving(100)))),
        Answer::Verdict(QfcommitOutcome::Misbehaving(100))
    );
    for r in [
        Ok(Err(QcBlockError::Fault(NodeMemberError::Store(read())))),
        Err(OnVMembersError::Members(MemberError::View(read()))),
    ] {
        let Answer::ReadFailed(why) = answer(r) else {
            panic!("not a failed read")
        };
        assert!(why.contains("the list is withheld"), "{why}");
    }
    assert!(matches!(
        answer(Err(OnVMembersError::Params(ParamsOutOfDomain(LlmqType::LlmqTest)))),
        Answer::ReadFailed(_)
    ));
    for e in [QcBlockError::BlsUnsupported, QcBlockError::NoReason("no quorum")] {
        assert!(matches!(answer(Ok(Err(e))), Answer::NoVerdict(_)));
    }
}

/// A commitment in the pool is had, so its announcement is not requested, and is served by hash;
/// another is answered `notfound`.
#[test]
fn a_pooled_commitment_is_had_and_served() {
    let fake = Fake::new(Answer::NoAnswer);
    let (qc, payload, inv) = commitment();
    fake.pool.lock().unwrap().insert(commitment_hash(&qc), Arc::new(qc));
    let (mut sim, a, b) = sim(&fake);
    announce(&mut sim, &a, &[inv]);
    assert_eq!(got(&sim, &a, CommandString::GETDATA), []);
    let other = Inventory::new(InvType::QuorumFinalCommitment, Hash256::from_lendian([9; 32]));
    sim.deliver(&b, &[("getdata", encode(&GetData::new(vec![inv, other])))]);
    settle(&mut sim);
    let sent = sim.sent(&b);
    let served: Vec<_> = sent.iter().filter(|(c, _)| *c == CommandString::QFCOMMIT).collect();
    assert_eq!(served.len(), 1);
    assert_eq!(served[0].1, payload);
    let missing: Vec<_> = sent
        .into_iter()
        .filter(|(c, _)| *c == CommandString::NOTFOUND)
        .flat_map(|(_, p)| NotFound::decode(&mut &p[..]).unwrap().inventory().clone())
        .collect();
    assert_eq!(missing, [other]);
}

/// The gate's hash, the pool's key and the relayed inv are one hash: dash-consensus's
/// `commitment_hash`, which is base-sdk's `::SerializeHash(qc)` for a decoded commitment of every
/// version (A1 §1.3).
#[test]
fn a_commitments_hash_is_one_hash_for_every_version() {
    let p = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap();
    let t = LlmqType::LlmqTest;
    for version in 1..=4 {
        let qc =
            null_commitment(t, p.consensus.llmq(t).unwrap(), QuorumHash::from([5; 32]), version, 0);
        let decoded = Commitment::decode(&mut &encode(&qc)[..]).unwrap();
        assert_eq!(Hash256::from_lendian(commitment_hash(&decoded)), decoded.hash(), "v{version}");
    }
}
