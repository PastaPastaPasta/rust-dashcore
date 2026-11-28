use std::time::{Duration, Instant};

use dash_clock::MockClock;
use dash_kv::{AbortMark, RecordingSink};
use rand_core::Rng as _;

use super::*;
use crate::init::quorums::{QCacheThread, QuorumDb, init_quorums};
use crate::signing::RelayRecoveredSigs;

const WAIT: Duration = Duration::from_secs(30);

struct NoNet;

impl SigningNet for NoNet {
    fn misbehaving(&self, _: PeerId, _: i32) {}
    fn relay_inv(&self, _: &Inventory) {}
    fn relay_transaction(&self, _: &TxHash) {}
}

/// The pass's generator draws as Core's `FastRandomContext` does, draw for draw.
// Core: v24.0.2 src/llmq/signing.h:185
#[test]
fn the_signing_rng_draws_as_fast_random_context() {
    let seed = [7; 32];
    let (mut rng, mut core) =
        (SigningRng(FastRandom::with_seed(seed)), FastRandom::with_seed(seed));
    assert_eq!(rng.next_u64(), core.rand64());
    assert_eq!(rng.next_u32(), core.rand32());
    let (mut got, mut want) = ([0; 40], [0; 40]);
    rng.fill_bytes(&mut got);
    core.fill(&mut want);
    assert_eq!(got, want);
}

fn signing_over_unopened_recsigs() -> (NodeQuorums, NodeSigning) {
    let setup = crate::chain::tests::regtest();
    let mut hook = None;
    let quorums = init_quorums(&setup, QuorumDb::in_memory(), QCacheThread::new(), &mut hook);
    let relay = Arc::new(RelayRecoveredSigs::default());
    let clock = Arc::new(MockClock::at(1));
    let signing = NodeSigning::new(&quorums, setup, Arc::new(NoNet), relay, clock, None);
    (quorums, signing)
}

/// The first pass's cleanup reads the database: unopened, it is a storage fault, which reaches
/// `AbortNode` once with the fault's text; interrupt then stop joins the thread.
#[test]
fn a_recsigs_storage_fault_reaches_abort_node_once() {
    let (_quorums, mut signing) = signing_over_unopened_recsigs();
    let sink = Arc::new(RecordingSink::default());
    signing.start(Box::new(|| false), FastRandom::with_seed([1; 32]), sink.clone()).unwrap();
    let start = Instant::now();
    while sink.marks().is_empty() {
        assert!(start.elapsed() < WAIT, "no abort");
        std::thread::sleep(Duration::from_millis(10));
    }
    signing.interrupt();
    signing.stop();
    assert!(!signing.is_started());
    let marks = sink.marks();
    let [
        AbortMark::AbortNode {
            log,
            user: None,
        },
    ] = marks.as_slice()
    else {
        panic!("{marks:?}");
    };
    assert!(log.starts_with("recovered-signature storage fault: "), "{log}");
}

mod active {
    use std::sync::Mutex;

    use dash_args::ArgsManager;
    use dash_clock::NodeSeconds;
    use dash_evo::locks::RecoveredSig;
    use dash_params::LlmqType;
    use dashd_masternode::active::parse;
    use dashd_masternode::mnconn::QuorumNodes;
    use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
    use dashd_p2p::sim::Sim;
    use dashd_quorums::signing::api::{RecoveredSigIntake, RecoveredSigResult};
    use dashd_quorums::signing::{QuorumHandle, SignQueue};

    use super::*;
    use crate::init::recsigs::RecoveredSigsDb;

    type Aborts = Arc<Mutex<Vec<String>>>;

    /// Mock time, except that the share manager's cleaning thread panics on reading it.
    struct MaintPanics(MockClock);

    impl Clock for MaintPanics {
        fn node_now(&self) -> NodeSeconds {
            let thread = std::thread::current();
            assert_ne!(thread.name(), Some("sigsh-maint"), "maint read the clock");
            self.0.node_now()
        }

        fn steady(&self) -> Instant {
            self.0.steady()
        }
    }

    struct Rig {
        signing: NodeSigning,
        aborts: Aborts,
        _quorums: NodeQuorums,
        _sim: Sim,
    }

    /// `NetSigning` over an in-memory recovered-signature database at `clock`; on `masternode`,
    /// with step 7c's active parts, the share manager's abort path recording.
    fn rig(masternode: bool, clock: Arc<dyn Clock>) -> Rig {
        let setup = crate::chain::tests::regtest();
        let mut hook = None;
        let mut quorums =
            init_quorums(&setup, QuorumDb::in_memory(), QCacheThread::new(), &mut hook);
        quorums.recsigs = RecoveredSigsDb::in_memory();
        let identity = local_identity(&setup, &IdentityOptions::new()).unwrap();
        let sim = Sim::new(Arc::clone(&setup), identity);
        let aborts = Aborts::default();
        let args = ArgsManager::new();
        args.force_set_arg("-masternodeblsprivkey", &"01".repeat(32));
        let sink = Arc::clone(&aborts);
        let active = masternode.then(|| ActiveSigning {
            active: Arc::new(ActiveMasternode::new(parse(&args).unwrap().unwrap())),
            sporks: crate::sporks::spork_manager(&args, &setup, Arc::clone(&clock)).unwrap(),
            net: Arc::new(SharesNet {
                shared: Arc::clone(&sim.shared),
                core: Arc::clone(&sim.core),
                quorums: Arc::new(QuorumNodes::default()),
            }),
            legacy: Box::new(|| false),
            abort: Arc::new(move |what: &str| sink.lock().unwrap().push(what.to_owned())),
        });
        let relay = Arc::new(RelayRecoveredSigs::default());
        let signing = NodeSigning::new(&quorums, setup, Arc::new(NoNet), relay, clock, active);
        Rig {
            signing,
            aborts,
            _quorums: quorums,
            _sim: sim,
        }
    }

    fn mock() -> Arc<dyn Clock> {
        Arc::new(MockClock::at(1))
    }

    fn start(signing: &mut NodeSigning) {
        let sink = Arc::new(RecordingSink::default());
        signing.start(Box::new(|| false), FastRandom::with_seed([1; 32]), sink).unwrap();
    }

    struct Heard(&'static str, Arc<Mutex<Vec<&'static str>>>);

    impl RecoveredSigListener for Heard {
        fn handle_new_recovered_sig(&self, _: &RecoveredSig) -> RecoveredSigResult {
            self.1.lock().unwrap().push(self.0);
            RecoveredSigResult::Nothing
        }
    }

    struct NoQueue;

    impl SignQueue<QuorumHandle> for NoQueue {
        fn force_reannouncement(
            &self,
            _: &Arc<QuorumHandle>,
            _: LlmqType,
            _: &[u8; 32],
            _: &[u8; 32],
        ) {
        }
        fn async_sign(&self, _: Arc<QuorumHandle>, _: [u8; 32], _: [u8; 32]) {}
    }

    fn recsig(id: u8) -> Arc<RecoveredSig> {
        Arc::new(RecoveredSig {
            llmq_type: LlmqType::LlmqTest,
            quorum_hash: [4; 32].into(),
            id: [id; 32],
            msg_hash: [2; 32],
            sig: [0; 96],
        })
    }

    /// Only an active masternode has `ActiveContext`'s share manager, bound as the runtime's sign
    /// queue; a node that is not one keeps no share threads, no active-signer slot and no queue.
    // Core: v24.0.2 src/init.cpp:2163-2170, :2202
    #[test]
    fn only_an_active_masternode_builds_the_share_manager() {
        let mut plain = rig(false, mock());
        assert!(plain.signing.shares().is_none());
        let none = Arc::new(Mutex::new(Vec::new()));
        assert!(!plain.signing.add_active_signer(Arc::new(Heard("cl", none))));
        plain.signing.start_active();
        assert_eq!(plain.signing.active_listeners(), None);
        assert!(plain.signing.runtime().set_sign_queue(Arc::new(NoQueue)).is_ok());
        let mn = rig(true, mock());
        assert!(mn.signing.shares().is_some());
        assert_eq!(mn.signing.shares_started(), Some(false));
        assert!(mn.signing.runtime().set_sign_queue(Arc::new(NoQueue)).is_err());
    }

    /// The signing runtime holds the share manager weakly: after `Stop` and the node's drop, the
    /// share manager is gone, so a late sign request reaches no share manager.
    #[test]
    fn the_share_manager_is_dropped_after_stop_and_the_nodes_drop() {
        let mut rig = rig(true, mock());
        start(&mut rig.signing);
        rig.signing.start_active();
        rig.signing.stop_active();
        rig.signing.interrupt();
        rig.signing.stop();
        let weak = Arc::downgrade(rig.signing.shares().unwrap());
        let _queue_holder = Arc::clone(rig.signing.runtime());
        drop(rig);
        assert!(weak.upgrade().is_none());
    }

    /// `NetSigning::Start` starts the share threads after "recsigs", and `Stop` joins them after
    /// it; a node that is not a masternode runs "recsigs" alone.
    // Core: v24.0.2 src/llmq/net_signing.cpp:152-195
    #[test]
    fn the_share_threads_run_from_start_until_stop_after_recsigs() {
        for masternode in [false, true] {
            let mut rig = rig(masternode, mock());
            start(&mut rig.signing);
            assert!(rig.signing.is_started());
            assert_eq!(rig.signing.shares_started(), masternode.then_some(true));
            rig.signing.interrupt();
            rig.signing.stop();
            assert!(!rig.signing.is_started());
            assert_eq!(rig.signing.shares_started(), masternode.then_some(false));
            assert!(rig.aborts.lock().unwrap().is_empty());
        }
    }

    /// A panic on a share thread takes the share manager's abort path once; interrupt then stop
    /// still joins every thread.
    #[test]
    fn a_panic_on_a_share_thread_aborts() {
        let mut rig = rig(true, Arc::new(MaintPanics(MockClock::at(1))));
        start(&mut rig.signing);
        let start = Instant::now();
        while rig.aborts.lock().unwrap().is_empty() {
            assert!(start.elapsed() < WAIT, "no abort");
            std::thread::sleep(Duration::from_millis(10));
        }
        rig.signing.interrupt();
        rig.signing.stop();
        assert_eq!(*rig.aborts.lock().unwrap(), ["sigsh-maint panicked"]);
    }

    /// `ActiveContext::Start` registers the active signers in the order added, then the share
    /// manager; `Stop` unregisters all of them.
    // Core: v24.0.2 src/active/context.cpp:56-74
    #[test]
    fn active_signers_hear_recovered_sigs_in_order_from_start_until_stop() {
        let mut rig = rig(true, mock());
        let heard = Arc::new(Mutex::new(Vec::new()));
        for name in ["cl", "is"] {
            assert!(rig.signing.add_active_signer(Arc::new(Heard(name, Arc::clone(&heard)))));
        }
        let runtime = Arc::clone(rig.signing.runtime());
        assert!(runtime.process_recovered_sig(recsig(1), false).unwrap());
        assert!(heard.lock().unwrap().is_empty());
        rig.signing.start_active();
        assert_eq!(rig.signing.active_listeners(), Some(3));
        assert!(runtime.process_recovered_sig(recsig(2), false).unwrap());
        assert_eq!(*heard.lock().unwrap(), ["cl", "is"]);
        rig.signing.stop_active();
        assert_eq!(rig.signing.active_listeners(), Some(0));
        assert!(runtime.process_recovered_sig(recsig(3), false).unwrap());
        assert_eq!(*heard.lock().unwrap(), ["cl", "is"]);
    }

    /// The share manager's abort reaches `AbortNode` only while the node has not aborted, so an
    /// earlier abort's cause is the one reported.
    #[test]
    fn the_share_abort_keeps_an_earlier_aborts_cause() {
        let sink = Arc::new(RecordingSink::default());
        let shutdown = Arc::new(ShutdownState::new());
        let abort = share_abort(sink.clone(), Arc::clone(&shutdown));
        abort("first");
        shutdown.mark_aborted();
        abort("second");
        let marks = sink.marks();
        let [
            AbortMark::AbortNode {
                log,
                user: None,
            },
        ] = marks.as_slice()
        else {
            panic!("{marks:?}");
        };
        assert_eq!(log, "first");
    }
}
