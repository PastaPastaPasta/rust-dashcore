use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dash_clock::Clock;
use dash_coinjoin::pool::QUEUE_TIMEOUT;
use dash_coinjoin::queue::{MixingCounters, MixingMasternode, QueueEntry, QueueManager};
use dash_p2p::conn::Permissions;
use dash_p2p::policy::Throw;
use dash_p2p::{CommandString, OutMsg};
use dash_p2p_core::{DsQueue, GetData, Inv, InvType, Inventory, NotFound};
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_pkc::bls::core::BlsPkValue;
use dash_pkc::bls::{BlsScIetf, BlsSchemeKind, BlsSecretKey, BlsSigId};
use dash_primitives::{BlockHash, OutPoint, TxHash};
use dash_types::codec::BaseCodec;
use dash_types::{Hashable, Numeric};
use dashd_coinjoin::dsq::{DsqRuntime, already_have_queue, process_queue_getdata};
use dashd_coinjoin::env::ChainView;
use dashd_coinjoin::testing::FakeMnLists;
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::logic::ProcessorOptions;
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, Sim, SimPeer};
use dashd_p2p::{HandlerCx, NetHandler, Registry, Seams, Shared};

const DSQ: CommandString = CommandString::from_static("dsq");

type Runtime = DsqRuntime<Chain, FakeMnLists>;

struct Chain(AtomicBool);

impl ChainView for Chain {
    fn tip(&self) -> Option<(i32, BlockHash)> {
        None
    }
    fn v24_active_at_tip(&self) -> bool {
        false
    }
    fn v24_active_after_tip(&self) -> bool {
        false
    }
    fn has_chain_lock(&self, _height: i32, _hash: &BlockHash) -> bool {
        false
    }
    fn blockchain_synced(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

fn hash(n: u8) -> TxHash {
    TxHash::from_lendian([n; 32])
}

fn key() -> BlsSecretKey<BlsScIetf> {
    BlsSecretKey::from_ikm(&[7; 32]).unwrap()
}

fn queue(pro_tx: TxHash, time: i64, ready: bool) -> DsQueue {
    let unsigned = DsQueue::new(1 << 4, pro_tx, time, ready, Vec::new());
    let sig = key().sign_with(unsigned.signature_hash().as_bytes(), BlsSigId::Basic);
    DsQueue::new(unsigned.denom(), pro_tx, time, ready, sig.to_bytes().to_vec())
}

fn inventory(q: &DsQueue) -> Inventory {
    Inventory::new(InvType::Dsq, q.hash())
}

fn encode(value: &impl BaseCodec) -> Vec<u8> {
    let mut out = Vec::new();
    value.encode(&mut out);
    out
}

fn node(blocks_only: bool, registry: Registry) -> Sim {
    let params =
        Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let identity = local_identity(&params, &IdentityOptions::new()).unwrap();
    let sim = Sim::with(
        params,
        identity,
        ProcessorOptions {
            blocks_only,
            ..ProcessorOptions::default()
        },
        registry,
        Seams::default(),
        |_| {},
    );
    sim.chain.set_ibd(false);
    sim
}

fn runtime(relay_txs: bool) -> (Sim, Runtime, Arc<Chain>) {
    runtime_with_counters(relay_txs, Arc::default())
}

fn runtime_with_counters(
    relay_txs: bool,
    counters: Arc<Mutex<MixingCounters>>,
) -> (Sim, Runtime, Arc<Chain>) {
    let chain = Arc::new(Chain(AtomicBool::new(true)));
    let mut mns = FakeMnLists {
        enabled: 100,
        ..FakeMnLists::default()
    };
    for n in 1..=3 {
        mns.valid.insert(
            hash(n),
            MixingMasternode {
                pro_tx_hash: hash(n),
                collateral: OutPoint::new(hash(n + 10), 0),
                operator_key: BlsPkValue::from_canonical(
                    &key().public_key().to_bytes(),
                    BlsSchemeKind::Basic,
                )
                .unwrap(),
            },
        );
    }
    let runtime = DsqRuntime::new(relay_txs, Arc::clone(&chain), Arc::new(mns), counters);
    let mut registry = Registry::new();
    registry.register(Box::new(runtime.clone()));
    (node(!relay_txs, registry), runtime, chain)
}

fn peer(sim: &mut Sim, n: u8, version: i32, perms: Permissions) -> SimPeer {
    let p = sim.accept(n, perms).unwrap();
    sim.handshake(
        &p,
        PeerVersionMsg {
            version,
            ..PeerVersionMsg::v24(SIM_START)
        },
    );
    sim.pass();
    sim.sent(&p);
    p
}

fn deliver(sim: &mut Sim, p: &SimPeer, q: &DsQueue) {
    sim.deliver(p, &[("dsq", encode(q))]);
    sim.pass();
}

fn score(sim: &Sim, p: &SimPeer) -> i32 {
    sim.shared.misbehavior(p.handle.id()).unwrap().0
}

fn dsq_messages(sim: &Sim, p: &SimPeer) -> Vec<(CommandString, Vec<u8>)> {
    sim.sent(p).into_iter().filter(|(cmd, _)| *cmd == DSQ || *cmd == CommandString::INV).collect()
}

#[test]
fn relay_modes_below_and_at_inventory_version() {
    let mut sim = node(false, Registry::new());
    let q = queue(hash(1), SIM_START, false);
    for (n, version, enabled) in
        [(1, 70233, false), (2, 70233, true), (3, 70234, false), (4, 70234, true)]
    {
        let p = peer(&mut sim, n, version, Permissions::NONE);
        sim.deliver(&p, &[("senddsq", vec![u8::from(enabled)])]);
        sim.pass();
        sim.sent(&p);
        sim.shared.relay_dsq(&q);
        sim.pass();
        let sent = dsq_messages(&sim, &p);
        // Core v24.0.0 net_processing.cpp:4503-4513,2567-2583: NONE / ALL / INV; DSQ_INV_VERSION=70234.
        let expected = if !enabled {
            vec![]
        } else if version < 70234 {
            vec![(DSQ, encode(&q))]
        } else {
            vec![(CommandString::INV, encode(&Inv::new(vec![inventory(&q)])))]
        };
        assert_eq!(sent, expected, "version={version}, enabled={enabled}");
        // Core v24.0.0 net_processing.cpp:4503-4513: disabling resets either enabled mode to NONE.
        sim.deliver(&p, &[("senddsq", vec![0])]);
        sim.pass();
        sim.shared.relay_dsq(&queue(hash(1), SIM_START + 1, false));
        sim.pass();
        assert!(dsq_messages(&sim, &p).is_empty());
    }
}

#[test]
fn relay_modes_remain_independent_across_connected_peers() {
    let mut sim = node(false, Registry::new());
    let none = peer(&mut sim, 1, 70234, Permissions::NONE);
    let all = peer(&mut sim, 2, 70233, Permissions::NONE);
    let inv = peer(&mut sim, 3, 70234, Permissions::NONE);
    for p in [&all, &inv] {
        sim.deliver(p, &[("senddsq", vec![1])]);
        sim.pass();
        sim.sent(p);
    }
    let first = queue(hash(1), SIM_START, false);
    sim.shared.relay_dsq(&first);
    sim.pass();
    assert_eq!(
        [dsq_messages(&sim, &none), dsq_messages(&sim, &all), dsq_messages(&sim, &inv)],
        [
            vec![],
            vec![(DSQ, encode(&first))],
            vec![(CommandString::INV, encode(&Inv::new(vec![inventory(&first)])))],
        ]
    );
    sim.deliver(&inv, &[("senddsq", vec![0])]);
    sim.pass();
    let second = queue(hash(1), SIM_START + 1, false);
    sim.shared.relay_dsq(&second);
    sim.pass();
    assert_eq!(
        [dsq_messages(&sim, &none), dsq_messages(&sim, &all), dsq_messages(&sim, &inv)],
        [vec![], vec![(DSQ, encode(&second))], vec![]]
    );
}

#[test]
fn client_scores_null_unknown_and_bad_signature() {
    for (n, expected) in [(0, 100), (99, 10), (1, 10)] {
        let (mut sim, runtime, _) = runtime(true);
        let p = peer(&mut sim, 1, 70242, Permissions::NO_BAN);
        let pro_tx = if n == 0 {
            TxHash::default()
        } else {
            hash(n)
        };
        let q = DsQueue::new(1 << 4, pro_tx, SIM_START, false, vec![0; 96]);
        deliver(&mut sim, &p, &q);
        // Core v24.0.0 coinjoin/walletman.cpp:261-272,300-302: null=100, unknown=10, bad sig=10.
        assert_eq!(score(&sim, &p), expected);
        assert!(runtime.is_empty());
        assert!(dsq_messages(&sim, &p).is_empty());
    }
}

#[test]
fn duplicate_time_bounds_and_threshold_are_silent() {
    let (mut sim, runtime, _) = runtime(true);
    let p = peer(&mut sim, 1, 70242, Permissions::NONE);
    let q = queue(hash(1), SIM_START, false);
    deliver(&mut sim, &p, &q);
    // Core v24.0.0 coinjoin/walletman.cpp:318-326: the first queue is retained without a wallet.
    assert_eq!(runtime.len(), 1);
    deliver(&mut sim, &p, &q);
    deliver(&mut sim, &p, &queue(hash(1), SIM_START + 1, false));
    // Core v24.0.0 coinjoin/walletman.cpp:279-286: hash and same-MN/readiness duplicates are silent.
    assert_eq!(runtime.len(), 1);
    for time in [SIM_START - QUEUE_TIMEOUT - 1, SIM_START + QUEUE_TIMEOUT + 1] {
        deliver(&mut sim, &p, &queue(hash(2), time, false));
    }
    // Core v24.0.0 coinjoin/walletman.cpp:291: both time bounds silently drop.
    assert_eq!(runtime.len(), 1);
    deliver(&mut sim, &p, &queue(hash(1), SIM_START, true));
    // Core v24.0.0 coinjoin/walletman.cpp:305-318: no session submits; the MN exceeds its threshold.
    assert_eq!(runtime.len(), 1);
    assert_eq!(score(&sim, &p), 0);
    assert!(dsq_messages(&sim, &p).is_empty());
}

fn assert_rejected_queue(pro_tx: TxHash, time: i64, ready: bool, expected_score: i32) {
    let counters = Arc::new(Mutex::new(MixingCounters::default()));
    let (mut sim, runtime, _) = runtime_with_counters(true, Arc::clone(&counters));
    let p = peer(&mut sim, 1, 70242, Permissions::NO_BAN);
    let retained = queue(hash(1), SIM_START, false);
    deliver(&mut sim, &p, &retained);
    let before = counters.lock().unwrap().clone();
    assert!(before.is_mixing_threshold_exceeded(&hash(1), 100));
    sim.sent(&p);
    let rejected = DsQueue::new(1 << 4, pro_tx, time, ready, vec![0; 96]);
    deliver(&mut sim, &p, &rejected);
    assert_eq!(score(&sim, &p), expected_score);
    assert!(dsq_messages(&sim, &p).is_empty());
    assert_eq!(runtime.len(), 1);
    assert_eq!(runtime.get(&retained.hash()), Some(retained));
    assert_eq!(runtime.get(&rejected.hash()), None);
    assert_eq!(*counters.lock().unwrap(), before);
}

#[test]
fn duplicate_precedes_signature_check() {
    assert_rejected_queue(hash(1), SIM_START + 1, false, 0);
}

#[test]
fn past_time_bound_precedes_signature_check() {
    assert_rejected_queue(hash(2), SIM_START - QUEUE_TIMEOUT - 1, false, 0);
}

#[test]
fn future_time_bound_precedes_signature_check() {
    assert_rejected_queue(hash(2), SIM_START + QUEUE_TIMEOUT + 1, false, 0);
}

#[test]
fn signature_check_precedes_mixing_threshold() {
    assert_rejected_queue(hash(1), SIM_START, true, 10);
}

#[test]
fn unknown_masternode_precedes_time_bounds() {
    assert_rejected_queue(hash(99), SIM_START - QUEUE_TIMEOUT - 1, false, 10);
}

#[test]
fn ready_queue_without_wallet_is_stored_and_never_relayed() {
    let (mut sim, runtime, _) = runtime(true);
    let source = peer(&mut sim, 1, 70242, Permissions::NONE);
    let observer = peer(&mut sim, 2, 70234, Permissions::NONE);
    let legacy_observer = peer(&mut sim, 3, 70233, Permissions::NONE);
    for p in [&observer, &legacy_observer] {
        sim.deliver(p, &[("senddsq", vec![1])]);
        sim.pass();
        sim.sent(p);
    }
    let q = queue(hash(1), SIM_START, true);
    deliver(&mut sim, &source, &q);
    sim.pass();
    // Core v24.0.0 coinjoin/walletman.cpp:305-326: no loaded wallet submits or relays; it retains even ready queues.
    assert_eq!(runtime.get(&q.hash()), Some(q));
    assert!(dsq_messages(&sim, &source).is_empty());
    assert!(dsq_messages(&sim, &observer).is_empty());
    assert!(dsq_messages(&sim, &legacy_observer).is_empty());
}

#[test]
fn intake_sync_gate_precedes_decode() {
    let (mut sim, runtime, chain) = runtime(true);
    let p = peer(&mut sim, 1, 70242, Permissions::NONE);
    chain.0.store(false, Ordering::SeqCst);
    assert!(sim.admitted(&p, "dsq", vec![0]).is_ok());
    sim.pass();
    deliver(&mut sim, &p, &queue(hash(1), SIM_START, false));
    // Core v24.0.0 coinjoin/walletman.cpp:251-256: unsynced returns before reading any payload.
    assert!(runtime.is_empty());
    assert_eq!(score(&sim, &p), 0);
    chain.0.store(true, Ordering::SeqCst);
    deliver(&mut sim, &p, &queue(hash(1), SIM_START, false));
    assert_eq!(runtime.len(), 1);
}

fn getdata(sim: &mut Sim, p: &SimPeer, invs: Vec<Inventory>) -> Vec<(CommandString, Vec<u8>)> {
    sim.deliver(p, &[("getdata", encode(&GetData::new(invs)))]);
    sim.pass();
    sim.pass();
    sim.sent(p)
        .into_iter()
        .filter(|(cmd, _)| *cmd == DSQ || *cmd == CommandString::NOTFOUND)
        .collect()
}

#[test]
fn wallet_inventory_serves_known_and_notfound_for_unknown() {
    let (mut sim, runtime, _) = runtime(true);
    let p = peer(&mut sim, 1, 70242, Permissions::NONE);
    let q = queue(hash(1), SIM_START, false);
    let unknown = inventory(&queue(hash(2), SIM_START, false));
    deliver(&mut sim, &p, &q);
    sim.sent(&p);
    // Core v24.0.0 net_processing.cpp:2455-2456: known queue inventory is already held.
    assert!(runtime.already_have(&inventory(&q)));
    assert!(!runtime.already_have(&unknown));
    assert!(!runtime.already_have(&Inventory::new(InvType::Tx, q.hash())));
    let replies = getdata(&mut sim, &p, vec![inventory(&q), unknown]);
    // Core v24.0.0 net_processing.cpp:3061-3066,3082-3085,3099-3110: known DSQ served, missing DSQ is notfound.
    assert_eq!(
        replies,
        vec![(DSQ, encode(&q)), (CommandString::NOTFOUND, encode(&NotFound::new(vec![unknown])))]
    );
}

struct MnQueues(QueueManager);

impl NetHandler for MnQueues {
    fn commands(&self) -> &'static [CommandString] {
        &[]
    }
    fn process_message(
        &mut self,
        _cx: &mut HandlerCx<'_>,
        _cmd: &CommandString,
        _payload: &[u8],
    ) -> Result<(), Throw> {
        Ok(())
    }
    fn already_have(&self, inv: &Inventory) -> bool {
        already_have_queue(&self.0, inv)
    }
    fn process_getdata(&mut self, cx: &mut HandlerCx<'_>, inv: &Inventory) -> bool {
        process_queue_getdata(&self.0, cx, inv)
    }
}

#[test]
fn masternode_inventory_helpers_serve_and_report_membership() {
    let q = queue(hash(1), SIM_START, false);
    let unknown = inventory(&queue(hash(2), SIM_START, false));
    let mut queues = QueueManager::default();
    queues.add(QueueEntry::new(q.clone(), OutPoint::new(hash(11), 0)));
    // Core v24.0.0 coinjoin/server.cpp:1445-1447: only a held MSG_DSQ is already held.
    assert!(already_have_queue(&queues, &inventory(&q)));
    assert!(!already_have_queue(&queues, &unknown));
    assert!(!already_have_queue(&queues, &Inventory::new(InvType::Tx, q.hash())));
    let mut registry = Registry::new();
    registry.register(Box::new(MnQueues(queues)));
    let mut sim = node(false, registry);
    let p = peer(&mut sim, 1, 70242, Permissions::NONE);
    // Core v24.0.0 coinjoin/server.cpp:1450-1458, net_processing.cpp:3082-3085,3099-3110: serve or notfound.
    assert_eq!(
        getdata(&mut sim, &p, vec![inventory(&q), unknown]),
        vec![(DSQ, encode(&q)), (CommandString::NOTFOUND, encode(&NotFound::new(vec![unknown])))]
    );
}

#[test]
fn blocks_only_has_no_queue_manager_even_for_relay_permission() {
    let (mut sim, runtime, _) = runtime(false);
    let p = peer(&mut sim, 1, 70242, Permissions::RELAY);
    let q = queue(hash(1), SIM_START, false);
    deliver(&mut sim, &p, &q);
    // Core v24.0.0 coinjoin/walletman.cpp:109,133-137,237-240: no manager or schedule, regardless of peer permissions.
    assert!(runtime.is_empty());
    assert!(runtime.get(&q.hash()).is_none());
    assert!(!runtime.already_have(&inventory(&q)));
    assert!(runtime.schedule(sim.clock.steady()).is_none());
    assert_eq!(
        getdata(&mut sim, &p, vec![inventory(&q)]),
        vec![(CommandString::NOTFOUND, encode(&NotFound::new(vec![inventory(&q)])))]
    );
}

#[test]
fn maintenance_uses_steady_schedule_and_sync_shutdown_gates() {
    let (mut sim, runtime, chain) = runtime(true);
    let p = peer(&mut sim, 1, 70242, Permissions::NONE);
    let q = queue(hash(1), SIM_START, false);
    deliver(&mut sim, &p, &q);
    let start = sim.clock.steady();
    let mut task = runtime.schedule(start).unwrap();
    sim.clock.set_mock_time(SIM_START + QUEUE_TIMEOUT + 1);
    // Core v24.0.0 coinjoin/walletman.cpp:133-137: changing wall time alone does not make the one-second scheduler due.
    assert!(!task.tick(start, sim.core.adjusted_time(), false));
    assert_eq!(runtime.len(), 1);
    chain.0.store(false, Ordering::SeqCst);
    sim.clock.advance(Duration::from_secs(1));
    assert!(task.tick(sim.clock.steady(), sim.core.adjusted_time(), false));
    // Core v24.0.0 coinjoin/walletman.cpp:210-214: a due unsynced maintenance tick retains expired queues.
    assert_eq!(runtime.len(), 1);
    chain.0.store(true, Ordering::SeqCst);
    sim.clock.advance(Duration::from_secs(1));
    assert!(task.tick(sim.clock.steady(), sim.core.adjusted_time(), true));
    // Core v24.0.0 coinjoin/walletman.cpp:210-214: shutdown also retains them.
    assert_eq!(runtime.len(), 1);
    sim.clock.advance(Duration::from_secs(1));
    assert!(task.tick(sim.clock.steady(), sim.core.adjusted_time(), false));
    // Core v24.0.0 coinjoin/walletman.cpp:213, coinjoin/coinjoin.cpp:176-186: allowed maintenance removes expired queues.
    assert!(runtime.is_empty());
}

struct RelayFromHandler(Arc<Mutex<Option<Arc<Shared>>>>);

impl NetHandler for RelayFromHandler {
    fn commands(&self) -> &'static [CommandString] {
        &[DSQ]
    }
    fn process_message(
        &mut self,
        cx: &mut HandlerCx<'_>,
        _cmd: &CommandString,
        mut payload: &[u8],
    ) -> Result<(), Throw> {
        let q: DsQueue = cx.read(&mut payload)?;
        self.0.lock().unwrap().as_ref().unwrap().relay_dsq(&q);
        Ok(())
    }
}

#[test]
fn relay_from_handler_releases_source_lock_before_reading_modes() {
    let shared = Arc::new(Mutex::new(None));
    let mut registry = Registry::new();
    registry.register(Box::new(RelayFromHandler(Arc::clone(&shared))));
    let mut sim = node(false, registry);
    *shared.lock().unwrap() = Some(Arc::clone(&sim.shared));
    let source = peer(&mut sim, 1, 70233, Permissions::NONE);
    let none = peer(&mut sim, 2, 70234, Permissions::NONE);
    let inv = peer(&mut sim, 3, 70234, Permissions::NONE);
    for p in [&source, &inv] {
        sim.deliver(p, &[("senddsq", vec![1])]);
        sim.pass();
        sim.sent(p);
    }
    let q = queue(hash(1), SIM_START, false);
    deliver(&mut sim, &source, &q);
    sim.pass();
    // Core v24.0.0 net_processing.cpp:2573-2583: ALL includes the sender and sends the complete queue.
    assert_eq!(
        [dsq_messages(&sim, &none), dsq_messages(&sim, &source), dsq_messages(&sim, &inv)],
        [
            vec![],
            vec![(DSQ, encode(&q))],
            vec![(CommandString::INV, encode(&Inv::new(vec![inventory(&q)])))],
        ]
    );
}

#[test]
fn participant_push_requires_connected_matching_address() {
    let mut sim = node(false, Registry::new());
    let p = peer(&mut sim, 1, 70242, Permissions::NONE);
    let q = queue(hash(1), SIM_START, false);
    let msg = || OutMsg {
        command: DSQ,
        payload: encode(&q),
    };
    // Core v24.0.0 coinjoin/server.cpp:1312-1315: ForNode(CService) pushes to the connected participant only.
    assert!(sim.shared.push_to_addr(&p.handle.addr().service, msg()));
    assert_eq!(dsq_messages(&sim, &p), vec![(DSQ, encode(&q))]);
    let pending = sim.accept(2, Permissions::NONE).unwrap();
    assert!(!sim.shared.push_to_addr(&pending.handle.addr().service, msg()));
    sim.core.disconnect_by_id(p.handle.id());
    assert!(!sim.shared.push_to_addr(&p.handle.addr().service, msg()));
}

#[test]
fn already_held_queue_inventory_is_not_requested() {
    let (mut sim, _runtime, _) = runtime(true);
    let p = peer(&mut sim, 1, 70242, Permissions::NONE);
    let q = queue(hash(1), SIM_START, false);
    let held = inventory(&q);
    let missing = inventory(&queue(hash(2), SIM_START, false));
    deliver(&mut sim, &p, &q);
    sim.sent(&p);
    sim.deliver(&p, &[("inv", encode(&Inv::new(vec![held, missing])))]);
    while sim.pass() {}
    sim.clock.advance(Duration::from_secs(3));
    while sim.pass() {}
    let requested: Vec<_> = sim
        .sent(&p)
        .into_iter()
        .filter(|(cmd, _)| *cmd == CommandString::GETDATA)
        .flat_map(|(_, payload)| GetData::decode(&mut &payload[..]).unwrap().inventory().clone())
        .collect();
    // Core v24.0.0 net_processing.cpp:2455-2456,6841-6892: AlreadyHave suppresses only the retained DSQ.
    assert_eq!(requested, vec![missing]);
}
