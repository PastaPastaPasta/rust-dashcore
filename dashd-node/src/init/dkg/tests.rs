use std::sync::atomic::{AtomicUsize, Ordering};

use dash_clock::{Clock, MockClock};
use dash_dml::blockprocessor::pool::null_commitment;
use dash_p2p::{ConnectionType, Permissions};
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_types::Numeric as _;
use dashd_chainstate::chain::{
    MemBlockFiles, NoFlush, NullConnector, RecordingAbort, genesis_block,
};
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::sim::Sim;

use super::*;
use crate::chain::Chain;
use crate::chain::tests::{branch, fresh, imported, regtest, startup_over};

fn v() -> (Arc<ChainSetup>, Chain) {
    let params = regtest();
    let genesis = i64::from(genesis_block(&params).header().raw().time());
    let clock: Arc<dyn Clock> = Arc::new(MockClock::at(genesis + 86_400));
    let abort = Arc::new(RecordingAbort::default());
    let mut v =
        Chain::start(fresh(&params, &clock, &abort, Box::new(MemBlockFiles::default()))).unwrap();
    startup_over(&mut v, Box::new(NullConnector), Box::new(NoFlush));
    imported(&mut v);
    (params, v)
}

#[derive(Default)]
struct CountedRead {
    calls: AtomicUsize,
    answer: Option<Result<(), String>>,
}

impl MembersRead for CountedRead {
    fn members(
        &self,
        _: LlmqType,
        _: &BlockHash,
    ) -> Result<Option<QuorumMembers<Arc<Dmn>>>, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match &self.answer {
            None => Ok(None),
            Some(Ok(())) => Ok(Some(Arc::from([]))),
            Some(Err(e)) => Err(e.clone()),
        }
    }
}

fn base() -> TipRef {
    TipRef {
        height: 24,
        hash: BlockHash::from([3; 32]),
    }
}

/// Access #1 is one facade read per `all_members` call: the members as read, none once V has
/// stopped, and a failed read is the phase thread's fault.
// Core: v24.0.0 src/llmq/dkgsession.cpp:92 CDKGSession::Init
#[test]
fn facade_members_make_one_read_per_call() {
    let members = FacadeMembers(CountedRead {
        answer: Some(Ok(())),
        ..CountedRead::default()
    });
    assert!(members.all_members(LlmqType::LlmqTest, &base()).is_empty());
    assert_eq!(members.0.calls.load(Ordering::SeqCst), 1);
    assert!(members.all_members(LlmqType::LlmqTest, &base()).is_empty());
    assert_eq!(members.0.calls.load(Ordering::SeqCst), 2);
    let stopped = FacadeMembers(CountedRead::default());
    assert!(stopped.all_members(LlmqType::LlmqTest, &base()).is_empty());
    let failed = FacadeMembers(CountedRead {
        answer: Some(Err("read failed".into())),
        ..CountedRead::default()
    });
    let fault = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        failed.all_members(LlmqType::LlmqTest, &base())
    }));
    assert!(fault.is_err());
    assert_eq!(failed.0.calls.load(Ordering::SeqCst), 1);
}

/// Route B's members on a V without the members facade (a node without evo state): V drops the
/// read unrun, which the phase threads see as V stopped.
#[test]
fn route_b_members_read_none_without_a_facade() {
    let (params, mut v) = v();
    let genesis = genesis_block(&params).hash();
    let read = MembersRead::members(v.handle(), LlmqType::LlmqTest, &genesis);
    assert!(matches!(read, Ok(None)), "{read:?}");
    v.stop().unwrap();
}

/// `ancestor_hash` walks back from the tip's own entry, found by its hash: a tip on a branch the
/// active chain left gets its branch's ancestors, never the active chain's at that height.
// Core: v24.0.0 src/chain.cpp:85-107 CBlockIndex::GetAncestor
#[test]
fn route_b_chain_walks_back_from_the_tip_hash() {
    let (params, mut v) = v();
    let genesis = genesis_block(&params);
    let main = branch(&params, &genesis, 0, 3, 0);
    let side = branch(&params, &genesis, 0, 2, 1);
    for block in main.iter().chain(&side) {
        let r = v.handle().process_block(Arc::clone(block), true, None, true).recv();
        assert!(r.is_ok_and(|r| r.accepted));
    }
    let chain = RouteBChain {
        chain: v.handle().clone(),
        params: Arc::clone(&params),
    };
    let tip = chain.current_tip().expect("a tip");
    assert_eq!((tip.height, tip.hash), (3, main[2].hash()));
    let side_tip = chain.lookup(&QuorumHash::from(*side[1].hash().as_bytes())).expect("indexed");
    assert_eq!((side_tip.height, side_tip.hash), (2, side[1].hash()));
    assert_eq!(chain.ancestor_hash(&side_tip, 1), side[0].hash());
    assert_ne!(side[0].hash(), main[0].hash());
    assert_eq!(chain.ancestor_hash(&side_tip, 0), genesis.hash());
    assert_eq!(chain.ancestor_hash(&side_tip, 2), side[1].hash());
    assert_eq!(chain.ancestor_hash(&tip, 1), main[0].hash());
    assert!(chain.lookup(&QuorumHash::from([9; 32])).is_none());
    v.stop().unwrap();
}

/// The pure reads: DIP3, V19 and rotation from the chain parameters.
// Core: v24.0.0 src/deploymentstatus.h:14-18, :65-68; src/llmq/options.cpp:50-62
#[test]
fn route_b_chain_reads_deployments_from_the_parameters() {
    let (params, mut v) = v();
    let chain = RouteBChain {
        chain: v.handle().clone(),
        params: Arc::clone(&params),
    };
    let dip3 = params.consensus.dip0003_enforcement.0.to_u32() as i32;
    assert!(!chain.dip3_enforced(dip3 - 1) && chain.dip3_enforced(dip3));
    let v19 = params.consensus.deployment_height(BuriedDeployment::V19).to_u32() as i32;
    let at = |height| TipRef {
        height,
        hash: BlockHash::from([0; 32]),
    };
    assert!(!chain.v19_after(&at(v19 - 2)) && chain.v19_after(&at(v19 - 1)));
    let dip24 = params.consensus.deployment_height(BuriedDeployment::Dip0024).to_u32();
    let rotated = params
        .consensus
        .llmq_params()
        .map(|(_, p)| *p)
        .find(|p| p.use_rotation)
        .expect("regtest has a rotated type");
    let interval = u32::from(rotated.dkg_interval);
    let first = dip24.div_ceil(interval).max(1) * interval;
    assert!(chain.rotation_enabled(&rotated, &at(first as i32)));
    assert!(!chain.rotation_enabled(&rotated, &at(first as i32 - 1)));
    let plain = LlmqParams {
        use_rotation: false,
        ..rotated
    };
    assert!(!chain.rotation_enabled(&plain, &at(first as i32)));
    assert!(!chain.snapshot_unvalidated());
    v.stop().unwrap();
}

fn sim() -> Sim {
    let params =
        Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let identity = local_identity(&params, &IdentityOptions::new()).unwrap();
    Sim::new(params, identity)
}

/// `ForEachNode`'s peers: only the fully connected ones (`fSuccessfullyConnected` and not
/// `fDisconnect`), in the table's order.
// Core: v24.0.0 src/net.h:1353-1359 CConnman::FullyConnectedOnly
#[test]
fn conn_facts_take_only_fully_connected_peers_in_table_order() {
    let sim = sim();
    let peers: Vec<_> = (1..=5)
        .map(|n| {
            let peer = sim.connect(n, ConnectionType::Inbound, Permissions::NONE);
            assert!(sim.core.try_mark_verified(peer.handle.id(), [n; 32], [n; 32]));
            peer
        })
        .collect();
    for n in [4, 1, 2, 5] {
        peers[n - 1].handle.set_successfully_connected_for_tests();
    }
    sim.core.disconnect(peers[1].handle.id(), dash_p2p::Why::Discouraged);
    let seen: Vec<[u8; 32]> = PeerTable::fully_connected(&*sim.core)
        .iter()
        .map(|f| *f.verified_pro_tx.as_bytes())
        .collect();
    assert_eq!(seen, [[1; 32], [4; 32], [5; 32]]);
}

struct CountedPeers(AtomicUsize);

impl PeerTable for CountedPeers {
    fn fully_connected(&self) -> Vec<ConnFacts> {
        self.0.fetch_add(1, Ordering::SeqCst);
        vec![ConnFacts {
            verified_pro_tx: ProTxHash::from([1; 32]),
            version: 0,
            masternode_connection: false,
            relay_txs: false,
            services: 0,
        }]
    }
}

struct NoMeta;

impl MetaFacts for NoMeta {
    fn outbound_failed_too_many(&self, pro_tx: &ProTxHash) -> bool {
        pro_tx.as_bytes()[0] == 1
    }

    fn platform_banned(&self, pro_tx: &ProTxHash) -> bool {
        pro_tx.as_bytes()[0] == 2
    }
}

/// With PoSe off for the type (spork 23 at its default), the snapshot reads nothing else.
// Core: v24.0.0 src/active/dkgsession.cpp:259-261
#[test]
fn conn_facts_read_nothing_with_pose_off() {
    let params = regtest();
    let clock: Arc<dyn Clock> = Arc::new(MockClock::at(1_296_688_662));
    let sporks = crate::sporks::spork_manager(&dash_args::ArgsManager::new(), &params, clock)
        .expect("sporkman");
    let peers = Arc::new(CountedPeers(AtomicUsize::new(0)));
    let facts = PeerConnFacts {
        peers: Arc::clone(&peers) as _,
        sporks,
        meta: Arc::new(NoMeta),
    };
    let members = [ProTxHash::from([1; 32]), ProTxHash::from([2; 32])];
    assert_eq!(facts.snapshot(LlmqType::LlmqTest, &members), ConnSnapshot::default());
    assert_eq!(peers.0.load(Ordering::SeqCst), 0);
}

#[derive(Default)]
struct Relayed(Mutex<Vec<Inventory>>);

impl InvRelay for Relayed {
    fn relay_inv(&self, inv: &Inventory) {
        self.0.lock().unwrap().push(*inv);
    }
}

#[derive(Default)]
struct Pool(Mutex<Vec<[u8; 32]>>);

impl MineableAdd for Pool {
    fn add(&self, qc: &P2pCommitment) -> Option<[u8; 32]> {
        let hash = *qc.quorum_hash().as_bytes();
        let mut held = self.0.lock().unwrap();
        (!held.contains(&hash)).then(|| {
            held.push(hash);
            hash
        })
    }
}

/// The round's `AddMineableCommitment` then `PeerRelayInv`: added once, a duplicate is `None`,
/// and the inv, `MSG_QUORUM_FINAL_COMMITMENT`, goes out only for the added one. The pool's own
/// once-only answer is dashd-chainstate's `a_mineable_commitment_is_added_once`.
// Core: v24.0.0 src/llmq/blockprocessor.cpp:840-867, src/llmq/net_dkg.cpp:869-873
#[test]
fn a_mineable_commitment_is_added_once_and_relayed_once() {
    let params = regtest();
    let t = LlmqType::LlmqTest;
    let qc = null_commitment(t, params.consensus.llmq(t).unwrap(), QuorumHash::from([5; 32]), 1, 0);
    let relay = Arc::new(Relayed::default());
    let sink = NodeMineable {
        pool: Arc::new(Pool::default()),
        relay: Arc::clone(&relay) as _,
    };
    let mut added = Vec::new();
    for _ in 0..2 {
        let got = sink.add(&qc);
        if let Some(hash) = got {
            sink.relay_inv(&hash);
        }
        added.push(got);
    }
    let hash = added[0].expect("the pool took it");
    assert_eq!(added[1], None, "the duplicate");
    let relayed = relay.0.lock().unwrap().clone();
    assert_eq!(relayed, [Inventory::new(InvType::QuorumFinalCommitment, hash.into())]);
}

/// The wired pool on a V without the members facade takes nothing, so nothing is relayed.
#[test]
fn the_wired_pool_takes_nothing_without_a_facade() {
    let (params, mut v) = v();
    let t = LlmqType::LlmqTest;
    let qc = null_commitment(t, params.consensus.llmq(t).unwrap(), QuorumHash::from([5; 32]), 1, 0);
    assert_eq!(MineableAdd::add(v.handle(), &qc), None);
    v.stop().unwrap();
}

struct NoPeers;

impl PeerTable for NoPeers {
    fn fully_connected(&self) -> Vec<ConnFacts> {
        Vec::new()
    }
}

/// A masternode's view over [`NodeDkgStatus`]'s sessions: its proTxHash and the verified peers.
struct ActiveAt {
    inner: NodeDkgStatus,
    me: [u8; 32],
    connected: Vec<[u8; 32]>,
}

impl DkgStatusSource for ActiveAt {
    fn sessions(&self, detail: i32) -> UniValue {
        self.inner.sessions(detail)
    }

    fn active_pro_tx(&self) -> Option<[u8; 32]> {
        Some(self.me)
    }

    fn connected_pro_txs(&self) -> Vec<[u8; 32]> {
        self.connected.clone()
    }

    fn all_members_connected(&self, t: LlmqType) -> bool {
        self.inner.all_members_connected(t)
    }
}

/// `quorum dkgstatus` at one active session, against Core's writer: `session[]` with the type's
/// name and the listed status fields in `CDKGDebugSessionStatus::ToJson`'s order; then per entry
/// the type's name, the base's hash and each of `GetQuorumConnections`' members, in its set's
/// order, as `{proTxHash, connected, outbound}`; then each mineable commitment's numeric type,
/// hash and public key.
// Core: v24.0.0 src/rpc/quorums.cpp:336-454, src/llmq/debug.cpp:19-152, src/llmq/core_write.cpp:15-31
#[test]
fn dkgstatus_has_cores_shape_at_one_active_session() {
    let params = regtest();
    let t = LlmqType::LlmqTest;
    let llmqs: Vec<_> = params.consensus.llmq_params().map(|(t, p)| (t, *p)).collect();
    let debug = Arc::new(DebugManager::new(llmqs, || 1_700_000_000));
    let hash = QuorumHash::from([7; 32]);
    debug.init(t, 0, &hash, 24);
    debug.mark_phase_advanced(t, 0, dashd_quorums::dkg::QuorumPhase::Contribute);
    debug.update_member(t, 0, 1, |m| {
        m.received_contribution = true;
        true
    });
    let clock: Arc<dyn Clock> = Arc::new(MockClock::at(1_700_000_000));
    let members: Vec<[u8; 32]> = (1..=3).map(|i| [i; 32]).collect();
    let status = ActiveAt {
        inner: NodeDkgStatus {
            debug,
            src: NodeDebugSource {
                params: Arc::clone(&params),
                members: Arc::new(CountedRead::default()),
            },
            active: None,
            peers: Arc::new(NoPeers),
            sporks: crate::sporks::spork_manager(&dash_args::ArgsManager::new(), &params, clock)
                .unwrap(),
        },
        me: members[0],
        connected: vec![members[1]],
    };
    let llmq = *params.consensus.llmq(t).unwrap();
    let base = BlockHash::from([7; 32]);
    let reads = vec![crate::rpc::evo::ConnRead {
        llmq_type: t,
        params: llmq,
        base: Some((base, members.clone())),
    }];
    let pk = "ab".repeat(48);
    let mineable = vec![(t.to_base(), hash, pk.clone())];
    let ret = crate::rpc::evo::write_dkg_status(Some(&status), 0, (reads, mineable));

    use dashd_masternode::mnconn::{LlmqTopology, QuorumTopology as _};
    let all = LlmqTopology.quorum_connections(&llmq, &members, members[0], false, false);
    let outbound = LlmqTopology.quorum_connections(&llmq, &members, members[0], true, false);
    let mut all: Vec<_> = all.into_iter().collect();
    all.sort_unstable();
    assert!(!all.is_empty(), "a member connects to the others");
    let conns: Vec<String> = all
        .iter()
        .map(|m| {
            format!(
                r#"{{"proTxHash":"{}","connected":{},"outbound":{}}}"#,
                ProTxHash::from(*m),
                *m == members[1],
                outbound.contains(m)
            )
        })
        .collect();
    let expected = format!(
        concat!(
            r#"{{"session":[{{"llmqType":"llmq_test","status":{{"quorumHash":"{hash}","phase":2,"#,
            r#""receivedContributions":1,"receivedComplaints":0,"receivedJustifications":0,"#,
            r#""receivedPrematureCommitments":0}}}}],"#,
            r#""quorumConnections":[{{"llmqType":"llmq_test","quorumHash":"{base}","quorumConnections":[{conns}]}}],"#,
            r#""minableCommitments":[{{"llmqType":{t},"quorumHash":"{hash}","quorumPublicKey":"{pk}"}}]}}"#
        ),
        hash = hash,
        base = base,
        conns = conns.join(","),
        t = t.to_base(),
        pk = pk,
    );
    assert_eq!(ret.write(), expected);
}

/// A plain node has no session source: no `session` key, and the entries carry only the type.
// Core: v24.0.0 src/rpc/quorums.cpp:363-367, :386
#[test]
fn dkgstatus_off_a_masternode_has_no_sessions_or_connections() {
    let params = regtest();
    let t = LlmqType::LlmqTest;
    let reads = vec![crate::rpc::evo::ConnRead {
        llmq_type: t,
        params: *params.consensus.llmq(t).unwrap(),
        base: None,
    }];
    let ret = crate::rpc::evo::write_dkg_status(None, 0, (reads, Vec::new()));
    assert_eq!(
        ret.write(),
        r#"{"quorumConnections":[{"llmqType":"llmq_test"}],"minableCommitments":[]}"#
    );
}

/// `session[]` keeps each entry's `llmqType` and the six listed status fields, in Core's order.
#[test]
fn listed_sessions_keep_only_the_listed_fields() {
    let json = serde_json::json!({
        "time": 5,
        "timeStr": "x",
        "session": [{
            "llmqType": "llmq_test",
            "quorumIndex": 0,
            "status": {
                "llmqType": 100, "quorumHash": "ab", "quorumHeight": 24, "phase": 3,
                "sentContributions": true, "aborted": false, "badMembers": 0, "weComplain": 0,
                "receivedContributions": 2, "receivedComplaints": 1,
                "receivedJustifications": 0, "receivedPrematureCommitments": 0
            }
        }]
    });
    assert_eq!(
        listed_sessions(&json).write(),
        r#"[{"llmqType":"llmq_test","status":{"quorumHash":"ab","phase":3,"receivedContributions":2,"receivedComplaints":1,"receivedJustifications":0,"receivedPrematureCommitments":0}}]"#
    );
}

/// BL-945, CH5's q-1 interleaving: `d-q-tip` is still in tip 143's `CheckQuorumConnections` when
/// the DKG's worker takes tip 144. The DKG waits until the check for every tip posted before it
/// has finished, as Core's one validation-interface queue orders them, so the stale check runs
/// before the new session's `EnsureQuorumConnections` and cannot remove its connections.
// Core: v24.0.0 src/init.cpp:2167, :2210; src/llmq/net_quorum.cpp:390-405, :438-497
#[test]
fn a_dkg_tip_waits_for_the_quorum_checks_posted_before_it() {
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc::Receiver;

    use dashd_quorums::tip::{
        NetQuorumTip, QuorumConnectionsCheck, Tip, TipFault, TipSteps, TipWorker,
    };
    use dashd_quorums::{Role, TipRef};

    type Log = Arc<Mutex<Vec<String>>>;

    /// Tip 143's check parks on `gate`, as CH5's did on its V reads.
    struct GatedCheck {
        gate: Mutex<Receiver<()>>,
        log: Log,
    }

    impl QuorumConnectionsCheck for GatedCheck {
        fn check(&self, _: LlmqType, _: &LlmqParams, tip: &TipRef, _: &Role) {
            if tip.height == 143 {
                let _ = self.gate.lock().unwrap().recv();
            }
            self.log.lock().unwrap().push(format!("check {}", tip.height));
        }
    }

    struct NoSteps;

    impl TipSteps for NoSteps {
        fn cleanup_expired_data_requests(&self) {}
        fn trigger_recovery(&self, _: Role, _: &Tip) -> Result<(), TipFault> {
            Ok(())
        }
        fn start_cleanup_old_quorum_data(&self, _: &Tip) {}
    }

    struct Sessions(Log);

    impl ActiveDuties for Sessions {
        fn updated_block_tip(&self, new: BlockId, _: bool) {
            self.0.lock().unwrap().push(format!("dkg {}", new.0));
        }
    }

    let log = Log::default();
    let (release, gate) = channel();
    let p = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap();
    let llmqs = vec![(LlmqType::LlmqTest, *p.consensus.llmq(LlmqType::LlmqTest).unwrap())];
    let role = Role::Masternode {
        pro_tx: None,
        watching: false,
    };
    let tip = NetQuorumTip::new(Arc::new(move || role), Arc::new(|| true), llmqs, NoSteps)
        .with_connections_check(Arc::new(GatedCheck {
            gate: Mutex::new(gate),
            log: Arc::clone(&log),
        }));
    let resolve = Box::new(|id: BlockId| {
        Some(Tip {
            id,
            at: TipRef {
                height: id.0 as i32,
                hash: BlockHash::from([id.0 as u8; 32]),
            },
        })
    });
    let (worker, handle) =
        TipWorker::spawn(tip, resolve, Arc::new(AtomicBool::new(true)), Box::new(|_| {})).unwrap();
    let dkg = AfterQuorumTip {
        barrier: worker.barrier(),
        duties: Arc::new(Sessions(Arc::clone(&log))),
    };
    worker.updated_block_tip(BlockId(143), None, false);
    worker.updated_block_tip(BlockId(144), None, false);
    let (running, started) = channel();
    let dkg = std::thread::spawn(move || {
        let _ = running.send(());
        dkg.updated_block_tip(BlockId(144), false)
    });
    started.recv().unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    while log.lock().unwrap().is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    release.send(()).unwrap();
    dkg.join().unwrap();
    worker.close();
    handle.join().unwrap();
    assert_eq!(*log.lock().unwrap(), ["check 143", "check 144", "dkg 144"]);
}
