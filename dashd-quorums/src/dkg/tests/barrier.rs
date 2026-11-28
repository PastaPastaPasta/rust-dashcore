//! Item 7: a `getdata` lookup from another thread answers while the phase thread is held in
//! the middle of a batch (R-d; owner ruling Q1), as Core's `invCs` lets it.

use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use dash_llmq::dkg::reference::{Network, seeded_rng};
use dash_llmq::dkg::rng::SeededRng;
use dash_llmq::dkg::session::{
    DkgDebugSink, DkgEnv, DkgMsgKind, MemberStatusBit, SerialRunner, SessionStatusBit,
};
use dash_llmq::dkg::wire::QuorumHash;
use dash_params::LlmqType;

use super::fakes::{Trace, params, sessions, tip};
use crate::dkg::Served;

const WAIT: Duration = Duration::from_secs(60);

/// A debug status that holds the batch when member `at`'s contribution arrives, until the test
/// releases it.
struct HoldAt {
    at: usize,
    reached: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
}

impl DkgDebugSink for HoldAt {
    fn init_local_session_status(&mut self, _: LlmqType, _: i32, _: &QuorumHash, _: i32) {}
    fn update_local_session_status(&mut self, _: LlmqType, _: i32, _: SessionStatusBit) {}
    fn update_local_member_status(
        &mut self,
        _: LlmqType,
        _: i32,
        idx: usize,
        bit: MemberStatusBit,
    ) {
        if idx == self.at && bit == MemberStatusBit::ReceivedContribution {
            self.reached.send(()).expect("the test listens");
            self.release.recv_timeout(WAIT).expect("the test releases the batch");
        }
    }
    fn add_local_member_complaint(&mut self, _: LlmqType, _: i32, _: usize, _: usize) {}
}

#[test]
fn a_lookup_does_not_wait_for_a_running_batch() {
    let params = params(LlmqType::LlmqTest);
    let weights = || Box::new(SeededRng::new(0x99)) as _;
    let mut net = Network::new(LlmqType::LlmqTest, &params, 3, 0xba22, weights, seeded_rng);
    let served = Arc::new(Served::for_session(&net.nodes[1].session));
    net.nodes[1].session = net.nodes[1].session.clone().with_served(served.clone());
    let first = net.contribute(0).expect("a contribution");
    let second = net.contribute(2).expect("a contribution");

    let trace = Arc::new(Trace::default());
    let (sessions, _) = sessions(&trace);
    sessions.updated_block_tip(&tip(48), false);
    sessions.updated_block_tip(&tip(50), false);
    sessions.handlers()[0].set_session(served);

    let (reached_tx, reached) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let mut hold = HoldAt {
        at: 2,
        reached: reached_tx,
        release: release_rx,
    };
    let bls = net.bls;
    let node = &mut net.nodes[1];
    let queued: Vec<_> = node.queue.drain(..).collect();
    assert_eq!(queued.len(), 2);

    thread::scope(|s| {
        let phase = s.spawn(move || {
            let batch: Vec<_> = queued.iter().map(|(from, p)| (*from, &p[..])).collect();
            let mut env = DkgEnv {
                bls,
                rng: &mut *node.rng,
                weights: &mut *node.weights,
                key: &node.key,
                store: &mut node.store,
                debug: &mut hold,
                sim: &node.sim,
                runner: &SerialRunner,
                verify: &mut node.verify,
            };
            node.session.process_batch(DkgMsgKind::Contribution, &batch, &mut env)
        });
        reached.recv_timeout(WAIT).expect("the batch reaches member 2's contribution");

        let (answer, answered) = mpsc::channel();
        let sessions = &sessions;
        let h0 = *first.session_hash();
        s.spawn(move || {
            let got = sessions.get_contribution(&h0).map(|qc| *qc.pro_tx_hash());
            answer.send(got).expect("the test listens");
        });
        let got = answered.recv_timeout(WAIT).expect("the lookup answers mid-batch");
        assert_eq!(got, Some(*first.pro_tx_hash()), "the batch's first message is served");

        release.send(()).expect("the batch listens");
        let outcome = phase.join().expect("the batch ends");
        assert_eq!(outcome.relay.len(), 2);
    });
    assert!(sessions.get_contribution(second.session_hash()).is_some());
}
