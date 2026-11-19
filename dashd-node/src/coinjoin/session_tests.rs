//! The lock-order session: a full CoinJoin round through [`NodePeerSink`] over the sim's peer
//! manager, the handler on the message thread and the tick on a scheduler thread.

use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use dash_coinjoin::collateral::MempoolAccept;
use dash_coinjoin::denom::SMALLEST_DENOMINATION;
use dash_coinjoin::dstx::{PoolLimits, txid};
use dash_coinjoin::inouts::{CoinView, ViewCoin};
use dash_coinjoin::pool::{PoolMessage, PoolState, PoolStatusUpdate};
use dash_coinjoin::queue::MixingMasternode;
use dash_consensus::Tx;
use dash_consensus::hash::hash160;
use dash_consensus::script::push_data;
use dash_consensus::script::sighash::signature_hash;
use dash_p2p::conn::Permissions;
use dash_p2p::policy::Throw;
use dash_p2p::{CommandString, NetAddr};
use dash_p2p_core::{
    DsAccept, DsComplete, DsEntry, DsFinalTx, DsQueue, DsStatusUpdate, Inv, InvType, Inventory,
};
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_primitives::{BlockHash, OutPoint, Transaction, TxHash, TxIn, TxOut, TxType};
use dash_types::Hashable;
use dash_types::codec::BaseCodec;
use dashd_coinjoin::adapters::MetaManMixing;
use dashd_coinjoin::dstx::DstxRuntime;
use dashd_coinjoin::env::{ChainView, Mempool};
use dashd_coinjoin::server::{CoinJoinServer, SharedServer};
use dashd_coinjoin::testing::{FakeActiveMn, FakeMempool, FakeMnLists, FakeRandom};
use dashd_masternode::meta::MasternodeMetaMan;
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::registry::CoreHandler;
use dashd_p2p::processing::seams::{CoinJoinObjects, TxInfo};
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, Sim, SimPeer, SimTxPool};
use dashd_p2p::{HandlerCx, NetHandler, ProcessorOptions, Registry, Seams};
use secp256k1::{Message, PublicKey, SecretKey};

use super::adapters::{NodePeerSink, NodeServerClock};

const DSA: CommandString = CommandString::from_static("dsa");
const DSI: CommandString = CommandString::from_static("dsi");
const DSS: CommandString = CommandString::from_static("dss");
const DSQ: CommandString = CommandString::from_static("dsq");
const DSSU: CommandString = CommandString::from_static("dssu");
const DSF: CommandString = CommandString::from_static("dsf");
const DSC: CommandString = CommandString::from_static("dsc");
const DENOM: i32 = 16;
const PRO_TX: [u8; 32] = [9; 32];
/// Another masternode's, with operator key 8.
const OTHER_PRO_TX: [u8; 32] = [8; 32];
const PATIENCE: Duration = Duration::from_secs(60);

type Hook = Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>>;

/// Synced, V24 active, no tip; runs a hook armed by the test on its next sync read.
#[derive(Clone, Default)]
struct Chain(Hook);

impl ChainView for Chain {
    fn tip(&self) -> Option<(i32, BlockHash)> {
        None
    }
    fn v24_active_at_tip(&self) -> bool {
        true
    }
    fn v24_active_after_tip(&self) -> bool {
        true
    }
    fn has_chain_lock(&self, _: i32, _: &BlockHash) -> bool {
        false
    }
    fn blockchain_synced(&self) -> bool {
        let hook = self.0.lock().unwrap().take();
        if let Some(hook) = hook {
            hook();
        }
        true
    }
}

/// The recording fake, whose submitted transactions enter the sim's pool for announcement.
struct PoolMempool {
    fake: FakeMempool,
    pool: SimTxPool,
}

impl MempoolAccept for PoolMempool {
    fn test_accept(&self, tx: &Transaction) -> Option<i64> {
        self.fake.test_accept(tx)
    }
}

impl CoinView for PoolMempool {
    fn coin(&self, outpoint: &OutPoint) -> Option<ViewCoin> {
        self.fake.coin(outpoint)
    }
    fn is_instantsend_locked(&self, txid: &TxHash) -> bool {
        self.fake.is_instantsend_locked(txid)
    }
}

impl Mempool for PoolMempool {
    fn submit(&mut self, tx: &Transaction) -> bool {
        let accepted = self.fake.submit(tx);
        if accepted {
            let info = TxInfo {
                tx: Arc::new(Tx::new(tx.clone())),
                time_s: SIM_START,
            };
            self.pool.insert(info, 1, 0);
        }
        accepted
    }
    fn prioritise(&mut self, txid: &TxHash, fee_delta: i64) {
        self.fake.prioritise(txid, fee_delta);
    }
}

/// The DSTX store the announcement and `getdata` paths read.
struct Objects(Arc<DstxRuntime>);

impl CoinJoinObjects for Objects {
    fn dstx_by_hash(&self, hash: &TxHash) -> Option<Vec<u8>> {
        self.0.get(hash).map(|stored| encode(&stored.dstx))
    }
    fn queue_by_hash(&self, _: &[u8; 32]) -> Option<Vec<u8>> {
        None
    }
}

type Runtime = CoinJoinServer<
    Chain,
    FakeMnLists,
    FakeActiveMn,
    PoolMempool,
    NodePeerSink,
    NodeServerClock,
    FakeRandom,
    MetaManMixing,
>;
type Server = SharedServer<Runtime>;

/// The registry's handler, bound to the server once the sim's peer manager exists.
#[derive(Clone, Default)]
struct Late(Arc<OnceLock<Server>>);

impl Late {
    fn server(&self) -> Server {
        self.0.get().expect("bound before the first message").clone()
    }
}

impl NetHandler for Late {
    fn commands(&self) -> &'static [CommandString] {
        &[DSA, DSI, DSS, DSQ]
    }
    fn core_handler(&self) -> Option<CoreHandler> {
        Some(CoreHandler::CoinJoinServer)
    }
    fn process_message(
        &mut self,
        cx: &mut HandlerCx<'_>,
        cmd: &CommandString,
        payload: &[u8],
    ) -> Result<(), Throw> {
        self.server().process_message(cx, cmd, payload)
    }
}

fn encode(value: &impl BaseCodec) -> Vec<u8> {
    let mut out = Vec::new();
    value.encode(&mut out);
    out
}

fn script(tag: u8) -> Vec<u8> {
    let mut s = vec![0x76, 0xa9, 0x14];
    s.extend([tag; 20]);
    s.extend([0x88, 0xac]);
    s
}

fn input_secret(tag: u8) -> SecretKey {
    SecretKey::from_secret_bytes([tag; 32]).unwrap()
}

fn input_script(tag: u8) -> Vec<u8> {
    let key = input_secret(tag);
    let mut script = vec![0x76, 0xa9, 0x14];
    script.extend(hash160(&PublicKey::from_secret_key(&key).serialize()));
    script.extend([0x88, 0xac]);
    script
}

fn input_prevout(tag: u8) -> OutPoint {
    OutPoint::new(TxHash::from([50 + tag; 32]), 0)
}

/// `tx`'s input from participant `tag`, signed SIGHASH_ALL|ANYONECANPAY.
fn signed_input(tx: &Transaction, tag: u8) -> TxIn {
    let prevout = input_prevout(tag);
    let index = tx.inputs().iter().position(|i| i.prevout() == prevout).unwrap();
    let hash = signature_hash(&input_script(tag), tx, index, 0x81);
    let mut signature = secp256k1::ecdsa::sign(Message::from_digest(hash), &input_secret(tag))
        .serialize_der()
        .to_vec();
    signature.push(0x81);
    let mut script_sig = Vec::new();
    push_data(&mut script_sig, &signature);
    push_data(&mut script_sig, &PublicKey::from_secret_key(&input_secret(tag)).serialize());
    TxIn::new(prevout, script_sig.into(), tx.inputs()[index].sequence())
}

fn collateral(tag: u8) -> Transaction {
    Transaction::new(
        2,
        TxType::Spend,
        vec![TxIn::new(OutPoint::new(TxHash::from([tag; 32]), 0), Vec::new().into(), u32::MAX)],
        vec![TxOut::from_duffs(10_000, script(tag).into())],
        0,
        Vec::new(),
    )
}

fn entry(tag: u8) -> DsEntry {
    DsEntry::new(
        vec![TxIn::new(input_prevout(tag), Vec::new().into(), u32::MAX)],
        collateral(tag),
        vec![TxOut::from_duffs(SMALLEST_DENOMINATION, script(tag + 10).into())],
    )
}

fn mempool(pool: &SimTxPool) -> PoolMempool {
    let mut fake = FakeMempool {
        default_accept_fee: Some(0),
        default_submit: Some(true),
        ..FakeMempool::default()
    };
    for tag in 1..=2 {
        let collateral_coin = ViewCoin {
            out: TxOut::from_duffs(20_000, script(tag).into()),
            in_mempool: false,
        };
        fake.coins.insert(OutPoint::new(TxHash::from([tag; 32]), 0), collateral_coin);
        let input_coin = ViewCoin {
            out: TxOut::from_duffs(SMALLEST_DENOMINATION, input_script(tag).into()),
            in_mempool: false,
        };
        fake.coins.insert(input_prevout(tag), input_coin);
    }
    PoolMempool {
        fake,
        pool: pool.clone(),
    }
}

fn masternodes(active: &FakeActiveMn) -> FakeMnLists {
    let me = MixingMasternode {
        pro_tx_hash: TxHash::from(PRO_TX),
        collateral: active.outpoint,
        operator_key: dash_dml::testkit::operator_key(7, dash_pkc::bls::BlsSchemeKind::Basic).get(),
    };
    let other = MixingMasternode {
        pro_tx_hash: TxHash::from(OTHER_PRO_TX),
        collateral: OutPoint::new(TxHash::from([8; 32]), 0),
        operator_key: dash_dml::testkit::operator_key(8, dash_pkc::bls::BlsSchemeKind::Basic).get(),
    };
    let mut mns = FakeMnLists::default();
    mns.by_collateral.insert(active.outpoint, me.clone());
    mns.valid.insert(me.pro_tx_hash, me);
    mns.valid.insert(other.pro_tx_hash, other);
    mns.enabled = 10;
    mns
}

struct Session {
    sim: Sim,
    server: Server,
    hook: Hook,
    participants: [SimPeer; 2],
    observer: SimPeer,
}

fn session() -> Session {
    let params =
        Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let mut identity = local_identity(&params, &IdentityOptions::new()).unwrap();
    identity.masternode = true;
    let limits = PoolLimits {
        min_participants: 2,
        max_participants: 2,
    };
    let pool = SimTxPool::default();
    let dstx = Arc::new(DstxRuntime::new(limits));
    let late = Late::default();
    let mut registry = Registry::new();
    registry.register(Box::new(late.clone()));
    let seams = Seams {
        txpool: Arc::new(pool.clone()),
        coinjoin: Box::new(Objects(Arc::clone(&dstx))),
        ..Seams::default()
    };
    let mut sim = Sim::with(params, identity, ProcessorOptions::default(), registry, seams, |_| {});
    sim.chain.set_ibd(false);
    let active = FakeActiveMn {
        pro_tx_hash: TxHash::from(PRO_TX),
        signature: vec![0x5a; 96],
        ..FakeActiveMn::default()
    };
    let chain = Chain::default();
    let hook = Arc::clone(&chain.0);
    let runtime = CoinJoinServer::new(
        limits,
        chain,
        masternodes(&active),
        active,
        mempool(&pool),
        NodePeerSink::new(Arc::clone(&sim.shared)),
        NodeServerClock::new(Arc::clone(&sim.core)),
        FakeRandom {
            draws: vec![99, 0, 41],
            ..FakeRandom::default()
        },
        MetaManMixing(Arc::new(MasternodeMetaMan::new())),
        dstx,
    );
    let server = SharedServer::new(runtime);
    assert!(late.0.set(server.clone()).is_ok());
    let participants = [peer(&mut sim, 1), peer(&mut sim, 2)];
    let observer = peer(&mut sim, 3);
    sim.deliver(&observer, &[("senddsq", vec![1])]);
    sim.pass();
    Session {
        sim,
        server,
        hook,
        participants,
        observer,
    }
}

fn peer(sim: &mut Sim, n: u8) -> SimPeer {
    let p = sim.accept_from(n, NetAddr::ipv4([10, 0, 0, n]), Permissions::NO_BAN).unwrap();
    sim.handshake(&p, PeerVersionMsg::v24(SIM_START));
    sim.pass();
    sim.sent(&p);
    p
}

/// Each participant's `dsa`, then the session is queued.
fn accept(s: &mut Session) {
    for (tag, p) in (1..=2).zip(&s.participants) {
        let dsa = DsAccept::new(DENOM, collateral(tag), Some(0));
        s.sim.deliver(p, &[(DSA.as_str(), encode(&dsa))]);
        s.sim.pass();
    }
    assert_eq!(s.server.info().state, PoolState::Queue);
}

/// The scheduler's tick on its own thread, as W5 schedules it.
fn tick_on_scheduler(server: &Server) -> std::thread::JoinHandle<()> {
    let server = server.clone();
    std::thread::spawn(move || NodePeerSink::outside_server(|| server.tick(false)))
}

fn join(handle: std::thread::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || tx.send(handle.join().is_ok()));
    assert_eq!(rx.recv_timeout(PATIENCE), Ok(true), "the scheduler's tick deadlocked or panicked");
}

type Frame = (CommandString, Vec<u8>);

/// What the node sent `p` since the last read: its frames other than `inv`, and its invs.
fn split(sim: &Sim, p: &SimPeer) -> (Vec<Frame>, Vec<Inventory>) {
    let (invs, frames): (Vec<_>, Vec<_>) =
        sim.sent(p).into_iter().partition(|(cmd, _)| *cmd == CommandString::INV);
    let invs = invs
        .into_iter()
        .flat_map(|(_, bytes)| Inv::decode(&mut &bytes[..]).unwrap().inventory().clone())
        .collect();
    (frames, invs)
}

fn frames(sim: &Sim, p: &SimPeer) -> Vec<Frame> {
    split(sim, p).0
}

fn invs(sim: &Sim, p: &SimPeer) -> Vec<Inventory> {
    split(sim, p).1
}

fn status(session_id: i32, state: PoolState, message: PoolMessage) -> Frame {
    let update = DsStatusUpdate::new(
        session_id,
        state as i32,
        PoolStatusUpdate::Accepted as i32,
        message as i32,
    );
    (DSSU, encode(&update))
}

fn queue_inv(ready: bool) -> Inventory {
    let unsigned = DsQueue::new(DENOM, TxHash::from(PRO_TX), SIM_START, ready, vec![0x5a; 96]);
    Inventory::new(InvType::Dsq, unsigned.hash())
}

/// dsa → tick → dsi → dss, with every handler on the sim's message thread and the tick on a
/// scheduler thread, through [`NodePeerSink`]: the exact frames each peer receives.
fn run_session() {
    let mut s = session();
    accept(&mut s);
    let session_id = 42;
    for (p, _) in s.participants.iter().zip(1..) {
        assert_eq!(
            frames(&s.sim, p),
            [status(session_id, PoolState::Queue, PoolMessage::MsgNoErr)]
        );
    }
    s.sim.pass();
    assert_eq!(invs(&s.sim, &s.observer), [queue_inv(false)]);

    join(tick_on_scheduler(&s.server));
    assert_eq!(s.server.info().state, PoolState::AcceptingEntries);
    s.sim.pass();
    assert_eq!(invs(&s.sim, &s.observer), [queue_inv(true)]);

    for (tag, p) in (1..=2).zip(&s.participants) {
        s.sim.deliver(p, &[(DSI.as_str(), encode(&entry(tag)))]);
        s.sim.pass();
    }
    let final_tx = s.server.lock().session().final_tx().clone();
    let dsf = (DSF, encode(&DsFinalTx::new(session_id, final_tx.clone())));
    let accepted = status(session_id, PoolState::AcceptingEntries, PoolMessage::MsgEntriesAdded);
    let relayed = |state| status(session_id, state, PoolMessage::MsgNoErr);
    let signing = relayed(PoolState::Signing);
    assert_eq!(
        frames(&s.sim, &s.participants[0]),
        [accepted.clone(), relayed(PoolState::AcceptingEntries), dsf.clone(), signing.clone()]
    );
    assert_eq!(frames(&s.sim, &s.participants[1]), [accepted, dsf.clone(), signing]);

    for (tag, p) in (1..=2).zip(&s.participants) {
        let mut signed = vec![1];
        signed_input(&final_tx, tag).encode(&mut signed);
        s.sim.deliver(p, &[(DSS.as_str(), signed)]);
        s.sim.pass();
    }
    s.sim.pass();
    let dsc = (DSC, encode(&DsComplete::new(session_id, PoolMessage::MsgSuccess as i32)));
    let seen: Vec<_> = s
        .participants
        .iter()
        .chain([&s.observer])
        .map(|p| {
            let (frames, mut invs) = split(&s.sim, p);
            let want = if p.handle.id() == s.observer.handle.id() {
                vec![]
            } else {
                vec![dsc.clone()]
            };
            assert_eq!(frames, want);
            invs.sort_by_key(|inv| inv.inv_type() == InvType::Dstx);
            invs
        })
        .collect();
    let dstx_txid = TxHash::from(seen[0][1].inv_hash());
    let stored = s.server.lock().dstx().get(&dstx_txid).expect("the DSTX is committed");
    let signed: Vec<TxIn> = final_tx
        .inputs()
        .iter()
        .map(|i| {
            (1..=2).map(|tag| signed_input(&final_tx, tag)).find(|s| s.prevout() == i.prevout())
        })
        .collect::<Option<_>>()
        .unwrap();
    assert_eq!(*stored.dstx.tx().inputs(), signed);
    assert_eq!(stored.dstx.tx().outputs(), final_tx.outputs());
    assert_eq!(
        (stored.dstx.pro_tx_hash(), stored.dstx.sig().as_slice()),
        (TxHash::from(PRO_TX), &[0x5a; 96][..])
    );
    let dstx_inv = Inventory::new(InvType::Dstx, txid(stored.dstx.tx()).into());
    let collateral_inv = Inventory::new(InvType::Tx, txid(&collateral(1)).into());
    for seen in seen {
        assert_eq!(seen, [collateral_inv, dstx_inv]);
    }
    assert_eq!(s.server.info().state, PoolState::Idle);
    foreign_queue(&mut s);
}

/// Another masternode's valid queue, announced by one participant and sent by the other: the
/// handler erases and forgets the request in the pass and relays the queue to the observer.
// Core: coinjoin/server.cpp:128-185; net_processing.cpp:6974-6977, :6999-7002
fn foreign_queue(s: &mut Session) {
    let unsigned = DsQueue::new(DENOM, TxHash::from(OTHER_PRO_TX), SIM_START, false, Vec::new());
    let sig = dash_dml::testkit::operator_sig(8, unsigned.signature_hash().as_bytes());
    let sig = sig.to_bytes(dash_pkc::bls::BlsSchemeKind::Basic).to_vec();
    let dsq = DsQueue::new(DENOM, TxHash::from(OTHER_PRO_TX), SIM_START, false, sig);
    let inv = Inventory::new(InvType::Dsq, dsq.hash());
    let [sender, announcer] = &s.participants;
    s.sim.deliver(announcer, &[("inv", encode(&Inv::new(vec![inv])))]);
    s.sim.pass();
    let requests = s.sim.lp.logic().requested_objects();
    assert_eq!(requests.count(announcer.handle.id()), 1);
    let getdata = (CommandString::GETDATA, encode(&Inv::new(vec![inv])));
    assert_eq!(split(&s.sim, announcer), (vec![getdata], vec![]));
    s.sim.deliver(sender, &[(DSQ.as_str(), encode(&dsq))]);
    s.sim.pass();
    s.sim.pass();
    assert_eq!((requests.count(sender.handle.id()), requests.count(announcer.handle.id())), (0, 0));
    assert_eq!(invs(&s.sim, &s.observer), [inv]);
    for p in &s.participants {
        assert_eq!(split(&s.sim, p), (vec![], vec![]));
    }
    assert!(s.server.lock().session().queues().has_queue(&dsq.hash()));
}

#[test]
fn a_full_session_through_the_node_peer_sink_never_deadlocks() {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        run_session();
        tx.send(()).unwrap();
    });
    match rx.recv_timeout(PATIENCE * 4) {
        Ok(()) => {}
        Err(mpsc::RecvTimeoutError::Disconnected) => panic!("the session failed"),
        Err(mpsc::RecvTimeoutError::Timeout) => panic!("the session deadlocked"),
    }
}

/// The message thread holds a peer's state while it waits for the server's lock, as when a
/// `dsa` arrives during the scheduler's tick: the tick's queue relay must wait until the server's
/// lock is released, or neither thread moves.
#[test]
fn the_ticks_queue_relay_waits_for_the_server_lock_to_be_released() {
    let mut s = session();
    accept(&mut s);
    s.sim.pass();
    s.sim.sent(&s.observer);
    let (entered_tx, entered) = mpsc::channel();
    let (holding, holding_rx) = mpsc::channel::<()>();
    *s.hook.lock().unwrap() = Some(Box::new(move || {
        entered_tx.send(()).unwrap();
        holding_rx.recv().unwrap();
    }));
    let tick = tick_on_scheduler(&s.server);
    entered.recv_timeout(PATIENCE).expect("the tick reads the sync state under the lock");
    let server = s.server.clone();
    let took_server_lock = s.sim.shared.with_state(s.observer.handle.id(), move |_| {
        holding.send(()).unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            drop(server.lock());
            let _ = tx.send(());
        });
        rx.recv_timeout(PATIENCE / 3).is_ok()
    });
    assert_eq!(took_server_lock, Some(true), "the tick relayed under the server lock");
    join(tick);
    s.sim.pass();
    assert_eq!(invs(&s.sim, &s.observer), [queue_inv(true)]);
}
