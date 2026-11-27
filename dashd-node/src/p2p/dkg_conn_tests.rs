//! DK1-L3: a DKG round's init on the composed runtime reaches the node's quorum connections.
//! The DKG runtime (`DkgSessions`, `DkgRuntime` and its phase thread), `NodeQuorumConnections`,
//! the node's quorum registry, `mn_metaman`, the spork manager and step 12's masternode dial
//! policy are the node's; the network core is dashd-net's dial sim (no sockets, a virtual
//! clock), and the chain, the members and the session's store and verifier are fixed fakes.
// Core: v24.0.0 src/llmq/net_dkg.cpp:800-844, :880-888; src/llmq/net_quorum.cpp:766-835;
// src/net.cpp:3437-3600

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dash_clock::Clock;
use dash_dml::testkit::dmn_with;
use dash_dml::{Dmn, MnList, MnType};
use dash_evo::llmq::{LlmqCommitment, QuorumMembers};
use dash_evo::spork::SporkId;
use dash_evo::types::ProTxHash;
use dash_llmq::dkg::reference::{MemStore, RecordingVerifier, TestOperatorKey, quorum};
use dash_llmq::dkg::session::{
    DkgEnv, DkgMsgKind, DkgSession, DkgStore, FinalVerifier, OperatorKey, RelayTopology,
    Sender as MsgSender,
};
use dash_llmq::dkg::wire::QuorumHash as WireQuorumHash;
use dash_netaddr::{NetAddr, Service};
use dash_p2p::{ConnectionType, DialLoop, Target};
use dash_params::{ChainParamsBuilder, LlmqParams, LlmqType, Network, Release};
use dash_primitives::{AddrV1, BlockHash, NetInfo, NetInfoV1, QuorumHash, ServiceV1};
use dashd_masternode::mnconn::{LlmqTopology, QuorumTopology};
use dashd_masternode::sync::NodeSyncNotifier;
use dashd_net::sim::{DialEvent, DialSim};
use dashd_p2p::sim::SIM_START;
use dashd_quorums::LocalMasternode;
use dashd_quorums::dkg::seams::{
    BaseFacts, BatchProcessor, ContributionsCache, DkgChain, MembersAccess, MineableSink,
    PendingQueues, TipFacts,
};
use dashd_quorums::dkg::{
    DebugManager, DkgFailStop, DkgPorts, DkgRuntime, DkgSessions, RoundSeams, SessionKey,
    SessionSeams, SimRates,
};

use super::*;
use crate::init::dkg::{NodePhaseClock, NodeSporks, PeerConnFacts};
use crate::init::test_args;

/// Core's functional-test spork key (test_framework.py:1519).
const SPORK_KEY: &str = "-sporkkey=cP4EKFyJsHT39LDqgdcB43Y3YXjNyjb5Fuas1GQSeAtjnZWmZEQK";
const TEST: LlmqType = LlmqType::LlmqTest;
const BASE_HEIGHT: i32 = 24;

fn pro(n: u8) -> [u8; 32] {
    [n; 32]
}

fn service(n: u8) -> Service {
    Service::new(NetAddr::ipv4([10, 0, 0, n]), 19999)
}

/// A valid masternode `pro(n)` at [`service`]`(n)`.
fn listed(n: u8) -> Arc<Dmn> {
    dmn_with(n, MnType::Regular, |dmn, cold| {
        dmn.pro_tx_hash = pro(n).into();
        cold.net_info = Arc::new(NetInfo::Legacy(NetInfoV1::new(ServiceV1::new(
            AddrV1::from([10, 0, 0, n]),
            service(n).port,
        ))));
    })
}

fn base() -> TipFacts {
    TipFacts {
        height: BASE_HEIGHT,
        hash: BlockHash::from([0xb0; 32]),
    }
}

/// One block, the quorum's base, at the tip.
struct BaseChain;

impl DkgChain for BaseChain {
    fn snapshot_unvalidated(&self) -> bool {
        false
    }
    fn dip3_enforced(&self, _: i32) -> bool {
        true
    }
    fn lookup(&self, hash: &QuorumHash) -> Option<BaseFacts> {
        (hash.as_bytes() == base().hash.as_bytes()).then(base)
    }
    fn ancestor_hash(&self, tip: &TipFacts, height: i32) -> BlockHash {
        assert_eq!((tip.hash, height), (base().hash, BASE_HEIGHT), "only the base is asked for");
        base().hash
    }
    fn rotation_enabled(&self, _: &LlmqParams, _: &TipFacts) -> bool {
        false
    }
    fn v19_after(&self, _: &TipFacts) -> bool {
        true
    }
    fn current_tip(&self) -> Option<TipFacts> {
        Some(base())
    }
}

struct Members(QuorumMembers<Arc<Dmn>>);

impl MembersAccess for Members {
    fn all_members(&self, t: LlmqType, at: &BaseFacts) -> QuorumMembers<Arc<Dmn>> {
        assert_eq!((t, at.hash), (TEST, base().hash));
        Arc::clone(&self.0)
    }
}

/// `NodeQuorumConnections`' members read: the quorum's members at its base.
struct BaseMembers(Vec<[u8; 32]>);

impl QuorumScanRead for BaseMembers {
    fn scan(&self, _: LlmqType, _: &BlockHash, _: usize) -> Option<QuorumScan> {
        None
    }
    fn ancestor(&self, _: &BlockHash, _: i32) -> Option<[u8; 32]> {
        None
    }
    fn members(&self, t: LlmqType, at: &BlockHash) -> Option<Vec<[u8; 32]>> {
        (t == TEST && *at == base().hash).then(|| self.0.clone())
    }
}

struct Me(ProTxHash);

impl LocalMasternode for Me {
    fn pro_tx_hash(&self) -> Option<ProTxHash> {
        Some(self.0)
    }
}

struct Relay(Vec<ProTxHash>);

impl RelayTopology for Relay {
    fn relay_members(
        &mut self,
        _: LlmqType,
        _: &WireQuorumHash,
        me: &ProTxHash,
    ) -> BTreeSet<ProTxHash> {
        dash_llmq::topology::quorum_relay_members(&self.0, me, true)
    }
}

/// The session's operator key, and a store and verifier no step before Contribute reads.
struct Ports {
    key: TestOperatorKey,
    pros: Vec<ProTxHash>,
}

impl DkgPorts for Ports {
    fn topology(&self) -> Box<dyn RelayTopology + '_> {
        Box::new(Relay(self.pros.clone()))
    }
    fn store(&self) -> Box<dyn DkgStore + '_> {
        Box::new(MemStore::with_members(self.pros.clone()))
    }
    fn verifier(&self) -> Box<dyn FinalVerifier + '_> {
        Box::new(RecordingVerifier::default())
    }
    fn operator_key(&self) -> &dyn OperatorKey {
        &self.key
    }
}

/// No queue holds a message.
struct NoIntake;

impl PendingQueues for NoIntake {
    fn clear(&self, _: SessionKey) {}

    fn pop_batch(&self, _: SessionKey, _: DkgMsgKind, _: usize) -> Vec<(MsgSender<i64>, Vec<u8>)> {
        Vec::new()
    }

    fn enqueue_own(&self, _: SessionKey, _: DkgMsgKind, _: &ProTxHash, _: Vec<u8>) {}
}

impl BatchProcessor for NoIntake {
    fn process_batch(
        &self,
        _: &mut DkgSession<Arc<Dmn>>,
        _: &mut DkgEnv<'_>,
        _: DkgMsgKind,
        _: Vec<(MsgSender<i64>, Vec<u8>)>,
    ) {
    }
}

struct NoCache;

impl ContributionsCache for NoCache {
    fn cleanup_cache(&self) {}
}

struct NoMineable;

impl MineableSink for NoMineable {
    fn add(&self, _: &LlmqCommitment) -> Option<[u8; 32]> {
        None
    }
    fn relay_inv(&self, _: &[u8; 32]) {}
}

#[derive(Default)]
struct Faults(Mutex<Vec<String>>);

impl DkgFailStop for Faults {
    fn fail_stop(&self, _: SessionKey, why: &str) {
        self.0.lock().unwrap().push(why.to_owned());
    }
}

struct Quiet;

impl NodeSyncNotifier for Quiet {
    fn sync_reset(&self) {}

    fn sync_finished(&self) {}
}

/// A member of `members` for which `EnsureQuorumConnections` picks outbound quorum connections.
fn member_with_outbound(params: &LlmqParams, members: &[[u8; 32]]) -> ([u8; 32], Vec<[u8; 32]>) {
    members
        .iter()
        .map(|me| {
            let mut picked: Vec<_> = LlmqTopology
                .quorum_connections(params, members, *me, true, false)
                .into_iter()
                .collect();
            picked.sort_unstable();
            (*me, picked)
        })
        .find(|(_, picked)| !picked.is_empty())
        .expect("a member with outbound quorum connections")
}

/// DK1-L3 (MGR 11-21): the composed runtime's init tick for a quorum the local masternode is a
/// member of asks the node's `NodeQuorumConnections` for `EnsureQuorumConnections`' outbound
/// members, which go to the quorum registry, and with SPORK_23 on for
/// `AddQuorumProbeConnections`' probes, every other member, which go to the network core's
/// pending probes. The node's masternode dial loop then opens a masternode connection to each
/// registered member, one a pass, and probes any member left; once the outbound connections to
/// the registered members have verified (MNAUTH's effect on the peer, set here), the loop's next
/// pass records each one's `lastOutboundSuccess` at the pass's time and drops its probe.
// Core: v24.0.0 src/llmq/net_dkg.cpp:880-888, :800-844; src/llmq/net_quorum.cpp:766-835;
// src/net.cpp:3504-3531, :3538-3576
#[test]
fn core_a_dkg_init_requests_quorum_connections_and_probes_and_a_dial_records_success() {
    let p = Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let params = *p.consensus.llmq(TEST).unwrap();
    let members: Vec<[u8; 32]> = (1..=params.size as u8).map(pro).collect();
    let (me, picked) = member_with_outbound(&params, &members);
    let others: HashSet<[u8; 32]> = members.iter().copied().filter(|m| *m != me).collect();

    let mut sim = DialSim::new(Arc::clone(&p), |cfg| {
        cfg.dns_seed = false;
        cfg.fixed_seeds = false;
    });
    let clock: Arc<dyn Clock> = Arc::clone(&sim.clock) as _;
    let sporks =
        crate::sporks::spork_manager(&test_args(&[SPORK_KEY]), &p, Arc::clone(&clock)).unwrap();
    sporks.update_spork(SporkId::SPORK_17_QUORUM_DKG_ENABLED, 0).unwrap();
    sporks.update_spork(SporkId::SPORK_23_QUORUM_POSE, 0).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let metaman = Arc::new(MasternodeMetaMan::new());
    assert!(metaman.load_cache(&dir.path().join("mncache.dat"), [1, 2, 3, 4], false));
    let mut list = MnList::default_pre_dip3();
    for m in &members {
        list.insert_for_tests(listed(m[0]));
    }
    let tip = super::quorum_conn_tests::tip_of(list);
    let registry = Arc::new(QuorumNodes::default());
    let connections = Arc::new(NodeQuorumConnections::new(QuorumConnParts {
        params: Arc::clone(&p),
        chain: Arc::new(BaseMembers(members.clone())),
        registry: Arc::clone(&registry),
        core: Arc::clone(&sim.core),
        sporks: Arc::clone(&sporks),
        metaman: Arc::clone(&metaman),
        tip_list: tip.clone(),
        clock: Arc::clone(&clock),
        watch_seed: [5; 32],
    }));

    let llmqs = vec![(TEST, params)];
    let chain: Arc<dyn DkgChain> = Arc::new(BaseChain);
    let sessions = Arc::new(DkgSessions::new(
        llmqs.clone(),
        SessionSeams {
            chain: Arc::clone(&chain),
            sporks: Arc::new(NodeSporks(Arc::clone(&sporks))),
            cache: Arc::new(NoCache),
        },
    ));
    let pros: Vec<ProTxHash> = members.iter().map(|m| ProTxHash::from(*m)).collect();
    let dmns: Vec<Arc<Dmn>> = members.iter().map(|m| listed(m[0])).collect();
    let faults = Arc::new(Faults::default());
    let runtime = DkgRuntime::new(
        Arc::clone(&sessions),
        RoundSeams {
            chain,
            members: Arc::new(Members(dmns.into())),
            active: Arc::new(Me(ProTxHash::from(me))),
            ports: Arc::new(Ports {
                key: quorum(1, 7).1.remove(0),
                pros,
            }),
            queues: Arc::new(NoIntake),
            batches: Arc::new(NoIntake),
            connections: Arc::clone(&connections) as _,
            conn_facts: Arc::new(PeerConnFacts {
                peers: Arc::clone(&sim.core) as _,
                sporks: Arc::clone(&sporks),
                meta: Arc::clone(&metaman) as _,
            }),
            mineable: Arc::new(NoMineable),
            clock: Arc::new(NodePhaseClock::new(&p)),
            sim: Arc::new(SimRates::default()),
            debug: Arc::new(DebugManager::new(llmqs, || SIM_START)),
            fail_stop: Arc::clone(&faults) as _,
            watch: false,
        },
    );
    runtime.start().unwrap();
    sessions.updated_block_tip(&base(), false);
    let deadline = Instant::now() + Duration::from_secs(60);
    while sim.core.pending_probes().len() < others.len() {
        assert!(Instant::now() < deadline, "the init tick never asked for the probes");
        std::thread::sleep(Duration::from_millis(10));
    }
    runtime.stop();
    assert!(faults.0.lock().unwrap().is_empty(), "{:?}", faults.0.lock().unwrap());

    let groups = registry.quorum_groups();
    assert_eq!(groups.len(), 1, "one quorum registered");
    let mut registered = groups[0].clone();
    registered.sort_unstable();
    assert_eq!(registered, picked, "EnsureQuorumConnections' outbound members");
    let probes: HashSet<_> = sim.core.pending_probes().into_iter().collect();
    assert_eq!(probes, others, "AddQuorumProbeConnections: every member but me");

    let sync = Arc::new(MasternodeSync::new(Arc::clone(&sim.clock) as _, Box::new(Quiet)));
    sync.switch_to_next_asset();
    let policy = node_dial_policy(
        Arc::new(OutboundFlags::default()),
        &tip,
        Some(&sync),
        &registry,
        &metaman,
    );
    let by_service: HashMap<Service, [u8; 32]> =
        members.iter().map(|m| (service(m[0]), *m)).collect();
    let mut seen = 0;
    let mut verified_at = HashMap::new();
    for _ in 0..400 {
        sim.run(DialLoop::MnCon, &*policy, 1);
        for event in &sim.exec.events[seen..] {
            if let DialEvent::Opened {
                id,
                service,
                conn_type: ConnectionType::OutboundFullRelay,
            } = event
            {
                let member = by_service[service];
                if !picked.contains(&member) {
                    continue;
                }
                let peer = sim.core.peer(*id).unwrap();
                peer.set_successfully_connected_for_tests();
                assert!(sim.core.try_mark_verified(*id, member, [8; 32]));
                verified_at.insert(member, sim.clock.node_now().0);
            }
        }
        seen = sim.exec.events.len();
        if sim.core.pending_probes().is_empty() && verified_at.len() == picked.len() {
            break;
        }
    }
    let target = |r: &dashd_net::sim::OpenSummary| match &r.target {
        Target::Addr(a) => by_service[&a.service],
        other => panic!("a masternode dial by address: {other:?}"),
    };
    let mut quorum_dials: Vec<_> = sim
        .exec
        .requests
        .iter()
        .filter(|r| r.masternode && !r.masternode_probe)
        .map(target)
        .collect();
    quorum_dials.sort_unstable();
    assert_eq!(quorum_dials, picked, "one masternode connection per registered member");
    let probe_dials: HashSet<_> =
        sim.exec.requests.iter().filter(|r| r.masternode_probe).map(target).collect();
    let unpicked: HashSet<_> = others.iter().copied().filter(|m| !picked.contains(m)).collect();
    assert_eq!(probe_dials, unpicked, "the probes the quorum connections left");
    assert!(sim.core.pending_probes().is_empty());
    for m in &picked {
        let success = metaman.last_outbound_success(&ProTxHash::from(*m));
        assert!(success >= verified_at[m] && success > 0, "{m:?}: {success}");
        assert!(metaman.last_outbound_attempt(&ProTxHash::from(*m)) > 0);
    }
    for m in &unpicked {
        assert_eq!(metaman.last_outbound_success(&ProTxHash::from(*m)), 0, "probed, not verified");
    }
    assert_eq!(metaman.last_outbound_success(&ProTxHash::from(me)), 0);
}
