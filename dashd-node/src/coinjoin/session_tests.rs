//! The lock-order session: a full CoinJoin round through [`NodePeerSink`] over the sim's peer
//! manager, the handler on the message thread and the tick on a scheduler thread.

use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use dash_chain::BlockRef;
use dash_clock::{Clock, MockClock};
use dash_coinjoin::collateral::MempoolAccept;
use dash_coinjoin::denom::SMALLEST_DENOMINATION;
use dash_coinjoin::dstx::{PoolLimits, txid};
use dash_coinjoin::inouts::{CoinView, ViewCoin};
use dash_coinjoin::pool::{PoolMessage, PoolState, PoolStatusUpdate};
use dash_coinjoin::queue::MixingMasternode;
use dash_coinjoin::server::SignedInputs;
use dash_consensus::coins::Coin;
use dash_consensus::hash::hash160;
use dash_consensus::script::push_data;
use dash_consensus::script::sighash::signature_hash;
use dash_consensus::{Amount, BlockId, Tx, TxRef};
use dash_p2p::conn::Permissions;
use dash_p2p::policy::Throw;
use dash_p2p::{CommandString, NetAddr, Service};
use dash_p2p_core::{
    DsAccept, DsComplete, DsEntry, DsFinalTx, DsQueue, DsStatusUpdate, Inv, InvType, Inventory,
};
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_primitives::{BlockHash, OutPoint, Transaction, TxHash, TxIn, TxOut, TxType};
use dash_types::Hashable;
use dash_types::codec::BaseCodec;
use dashd_chainstate::chain::{
    ActorReply, MemBlockFiles, NoFlush, NullConnector, RecordingAbort, WaitError, genesis_block,
    reply,
};
use dashd_coinjoin::adapters::MetaManMixing;
use dashd_coinjoin::dstx::DstxRuntime;
use dashd_coinjoin::env::{ChainView, Mempool};
use dashd_coinjoin::server::{CoinJoinServer, ServerInfo};
use dashd_coinjoin::testing::{FakeActiveMn, FakeMempool, FakeMnLists, FakeRandom};
use dashd_masternode::meta::MasternodeMetaMan;
use dashd_mempool::accept::single::{success, unavailable};
use dashd_mempool::api::{MempoolHandle, RelayRead};
use dashd_mempool::testkit::FakeIsMempool;
use dashd_mempool::types::{MempoolAcceptResult, Package, PackageMempoolAcceptResult};
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::registry::{CoreHandler, HandlerResult};
use dashd_p2p::processing::seams::{CoinJoinObjects, TxInfo};
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, Sim, SimPeer, SimTxPool};
use dashd_p2p::{HandlerCx, NetHandler, ProcessorOptions, Registry, Seams};
use secp256k1::{Message, PublicKey, SecretKey};

use super::adapters::{NodePeerSink, NodeServerClock};
use super::mempool::NodeMempool;
use super::role::NodeServer;
use crate::chain::{Chain as V, tests as chain_tests};
use crate::rpc::CoinsRead;
use crate::scheduler::Scheduler;
use crate::shutdown::ShutdownState;
use crate::test_log::{Lines, Tap, tap};

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

type Runtime<P = PoolMempool> = CoinJoinServer<
    Chain,
    FakeMnLists,
    FakeActiveMn,
    P,
    NodePeerSink,
    NodeServerClock,
    FakeRandom,
    MetaManMixing,
>;
type Server<P = PoolMempool> = NodeServer<Runtime<P>>;

/// The registry's handler, bound to the server once the sim's peer manager exists.
struct Late<P = PoolMempool>(Arc<OnceLock<Server<P>>>);

impl<P> Clone for Late<P> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<P> Default for Late<P> {
    fn default() -> Self {
        Self(Arc::default())
    }
}

impl<P> Late<P> {
    fn server(&self) -> Server<P> {
        self.0.get().expect("bound before the first message").clone()
    }
}

impl<P: Mempool + Send + 'static> NetHandler for Late<P> {
    fn commands(&self) -> &'static [CommandString] {
        &[DSA, DSI, DSS, DSQ]
    }
    fn core_handler(&self) -> Option<CoreHandler> {
        Some(CoreHandler::CoinJoinServer)
    }
    fn process_message(
        &mut self,
        _: &mut HandlerCx<'_>,
        _: &CommandString,
        _: &[u8],
    ) -> Result<(), Throw> {
        unreachable!("the registry calls process_message_result")
    }
    fn process_message_result(
        &mut self,
        cx: &mut HandlerCx<'_>,
        cmd: &CommandString,
        payload: &[u8],
    ) -> Result<HandlerResult, Throw> {
        self.server().shared().clone().process_message_result(cx, cmd, payload)
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

struct Session<P = PoolMempool> {
    sim: Sim,
    server: Server<P>,
    hook: Hook,
    participants: [SimPeer; 2],
    observer: SimPeer,
}

fn session() -> Session {
    session_over(mempool)
}

/// [`session`] with the mempool `mempool` gives over the sim's pool.
fn session_over<P: Mempool + Send + 'static>(mempool: impl FnOnce(&SimTxPool) -> P) -> Session<P> {
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
    let server = NodeServer::new(runtime);
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
fn accept<P: Mempool + Send + 'static>(s: &mut Session<P>) {
    for (tag, p) in (1..=2).zip(&s.participants) {
        let dsa = DsAccept::new(DENOM, collateral(tag), Some(0));
        s.sim.deliver(p, &[(DSA.as_str(), encode(&dsa))]);
        s.sim.pass();
    }
    assert_eq!(s.server.info().state, PoolState::Queue);
}

/// The server's tick on its own thread.
fn tick_on_scheduler<P: Mempool + Send + 'static>(
    server: &Server<P>,
) -> std::thread::JoinHandle<()> {
    let server = server.clone();
    std::thread::spawn(move || server.tick(false))
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
    let final_tx = s.server.shared().lock().session().final_tx().clone();
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
    let stored = s.server.shared().lock().dstx().get(&dstx_txid).expect("the DSTX is committed");
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
    assert!(s.server.shared().lock().session().queues().has_queue(&dsq.hash()));
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
/// lock is released, or neither thread moves. The tick is the one step 10a schedules (the 1 s
/// task before SyncManager's), run by the scheduler's own loop.
// Core: init.cpp:2366; coinjoin/server.cpp:1421-1433
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
    let scheduler = Scheduler::new();
    let shutdown = Arc::new(ShutdownState::new());
    s.server.ticks().schedule(&scheduler, &shutdown, Arc::new(|_| {}));
    scheduler.mock_forward(Duration::from_secs(1));
    let tick = {
        let scheduler = scheduler.clone();
        std::thread::spawn(move || scheduler.service_queue())
    };
    entered.recv_timeout(PATIENCE).expect("the tick reads the sync state under the lock");
    let server = s.server.clone();
    let took_server_lock = s.sim.shared.with_state(s.observer.handle.id(), move |_| {
        holding.send(()).unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            drop(server.shared().lock());
            let _ = tx.send(());
        });
        rx.recv_timeout(PATIENCE / 3).is_ok()
    });
    scheduler.stop();
    assert_eq!(took_server_lock, Some(true), "the tick relayed under the server lock");
    join(tick);
    s.sim.pass();
    assert_eq!(invs(&s.sim, &s.observer), [queue_inv(true)]);
}

/// A pool that admits every transaction with no fee, unless the pool's unavailable answer is
/// planted for test-accepts or for admissions; it records each call as (txid, test-accept).
#[derive(Default)]
struct Planted {
    reads: FakeIsMempool,
    test_accepts: Mutex<Option<&'static str>>,
    admissions: Mutex<Option<&'static str>>,
    calls: Mutex<Vec<(TxHash, bool)>>,
}

impl Planted {
    fn answer(&self, tx: &TxRef, test_accept: bool) -> MempoolAcceptResult {
        self.calls.lock().unwrap().push((tx.txid(), test_accept));
        let planted = if test_accept {
            &self.test_accepts
        } else {
            &self.admissions
        };
        match *planted.lock().unwrap() {
            Some(text) => unavailable(text.into()),
            None => success(100, 0, 0, tx.txid()),
        }
    }

    /// The admissions asked for, test-accepts aside.
    fn admitted(&self) -> Vec<TxHash> {
        let calls = self.calls.lock().unwrap();
        calls.iter().filter(|(_, test_accept)| !test_accept).map(|(txid, _)| *txid).collect()
    }
}

impl MempoolHandle for Planted {
    fn accept_async(&self, tx: TxRef, _: Option<u64>) -> ActorReply<MempoolAcceptResult> {
        let (send, answer) = reply();
        send.send(self.answer(&tx, false));
        answer
    }

    fn accept(&self, tx: TxRef, _: bool) -> Result<MempoolAcceptResult, WaitError> {
        Ok(self.answer(&tx, false))
    }

    fn test_accept(&self, tx: TxRef, _: bool) -> Result<MempoolAcceptResult, WaitError> {
        Ok(self.answer(&tx, true))
    }

    fn process_new_package(
        &self,
        txns: Package,
        test_accept: bool,
    ) -> Result<PackageMempoolAcceptResult, WaitError> {
        self.reads.process_new_package(txns, test_accept)
    }

    fn prioritise(&self, txid: &TxHash, fee_delta: Amount) {
        self.reads.prioritise(txid, fee_delta);
    }

    fn post_prioritise(&self, txid: &TxHash, fee_delta: Amount) {
        self.reads.post_prioritise(txid, fee_delta);
    }

    fn add_unbroadcast(&self, txid: &TxHash) {
        self.reads.add_unbroadcast(txid);
    }

    fn remove_unbroadcast(&self, txid: &TxHash, unchecked: bool) {
        self.reads.remove_unbroadcast(txid, unchecked);
    }

    fn with_relay_read(&self, f: &mut dyn FnMut(&dyn RelayRead)) {
        self.reads.with_relay_read(f);
    }
}

/// The node's server mempool over [`Planted`], V and the participants' confirmed coins.
struct OverNode {
    v: V,
    abort: Arc<RecordingAbort>,
    pool: Arc<Planted>,
    lines: Arc<Lines>,
    _tap: Tap,
}

impl OverNode {
    fn new() -> Self {
        let params = chain_tests::regtest();
        let genesis = genesis_block(&params);
        let clock: Arc<dyn Clock> =
            Arc::new(MockClock::at(i64::from(genesis.header().time()) + 60));
        let abort = Arc::new(RecordingAbort::default());
        let files = Box::new(MemBlockFiles::default());
        let mut v = V::start(chain_tests::fresh(&params, &clock, &abort, files)).unwrap();
        chain_tests::startup_over(&mut v, Box::new(NullConnector), Box::new(NoFlush));
        chain_tests::imported(&mut v);
        let lines = Arc::new(Lines::default());
        let tap = tap(Arc::clone(&lines));
        Self {
            v,
            abort,
            pool: Arc::default(),
            lines,
            _tap: tap,
        }
    }

    fn mempool(&self) -> NodeMempool {
        let coins: CoinsRead = Arc::new(|outpoint: &OutPoint| {
            let coin = (1..=2).find_map(|tag| {
                let out = if *outpoint == OutPoint::new(TxHash::from([tag; 32]), 0) {
                    TxOut::from_duffs(20_000, script(tag).into())
                } else if *outpoint == input_prevout(tag) {
                    TxOut::from_duffs(SMALLEST_DENOMINATION, input_script(tag).into())
                } else {
                    return None;
                };
                Some(Coin::from_output(&out, 1, false))
            });
            let best = BlockRef {
                id: BlockId(0),
                hash: BlockHash::default(),
                height: 0,
            };
            Ok((coin, best))
        });
        let pool: Arc<dyn MempoolHandle> = Arc::clone(&self.pool) as _;
        NodeMempool::new(pool, self.v.handle().clone(), coins, None)
    }

    fn session(&self) -> Session<NodeMempool> {
        let mempool = self.mempool();
        session_over(move |_| mempool)
    }

    /// The server's lines carrying `text`.
    fn logged(&self, text: &str) -> Vec<String> {
        self.lines.messages().into_iter().filter(|m| m.contains(text)).collect()
    }

    /// Passes until the server logged `text`, then a few more; its lines carrying `text`.
    fn await_line(&self, s: &mut Session<NodeMempool>, text: &str) -> Vec<String> {
        let deadline = Instant::now() + PATIENCE;
        while self.logged(text).is_empty() {
            assert!(Instant::now() < deadline, "no line with {text:?}");
            s.sim.pass();
            std::thread::sleep(Duration::from_millis(5));
        }
        for _ in 0..5 {
            s.sim.pass();
        }
        self.logged(text)
    }

    /// V still runs and never stopped.
    fn stop(mut self) {
        assert!(self.v.stop().is_some(), "V runs");
        assert!(self.abort.messages().is_empty(), "V stopped: {:?}", self.abort.messages());
    }
}

fn service(p: &SimPeer) -> Service {
    p.handle.addr().service.clone()
}

fn dropped(step: &str, text: &str) -> String {
    format!("CoinJoin server: {step} dropped, the mempool could not answer: {text}")
}

/// Participant `tag`'s `dsa`, through the region or the server's blocking call.
fn dsa(s: &mut Session<NodeMempool>, tag: u8, deliver: bool) {
    let p = &s.participants[usize::from(tag) - 1];
    let dsa = DsAccept::new(DENOM, collateral(tag), Some(0));
    if deliver {
        s.sim.deliver(p, &[(DSA.as_str(), encode(&dsa))]);
    } else {
        let version = p.handle.common_version();
        s.server.shared().lock().process_dsa(service(p), version, &dsa);
    }
}

/// Participant `tag`'s `dsi`, likewise.
fn dsi(s: &mut Session<NodeMempool>, tag: u8, deliver: bool) {
    let p = &s.participants[usize::from(tag) - 1];
    if deliver {
        s.sim.deliver(p, &[(DSI.as_str(), encode(&entry(tag)))]);
    } else {
        s.server.shared().lock().process_dsi(service(p), entry(tag));
    }
}

/// Participant `tag`'s signature of `final_tx`, likewise.
fn dss(s: &mut Session<NodeMempool>, tag: u8, deliver: bool, final_tx: &Transaction) {
    let p = &s.participants[usize::from(tag) - 1];
    let input = signed_input(final_tx, tag);
    if deliver {
        let mut signed = vec![1];
        input.encode(&mut signed);
        s.sim.deliver(p, &[(DSS.as_str(), signed)]);
    } else {
        s.server.shared().lock().process_dss(service(p), SignedInputs::Inputs(vec![input]));
    }
}

fn signed_by(tag: u8) -> impl Fn(&TxIn) -> bool {
    move |i| i.prevout() == input_prevout(tag) && !i.script_sig().is_empty()
}

fn signed(s: &Session<NodeMempool>, tag: u8) -> bool {
    s.server.shared().lock().session().final_tx().inputs().iter().any(signed_by(tag))
}

/// Passes until `done`.
fn settle(s: &mut Session<NodeMempool>, done: impl Fn(&Session<NodeMempool>) -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !done(s) {
        assert!(Instant::now() < deadline, "the server never settled");
        s.sim.pass();
        std::thread::sleep(Duration::from_millis(5));
    }
    s.sim.pass();
}

/// Drops what the node sent everyone so far.
fn drain(s: &mut Session<NodeMempool>) {
    for p in s.participants.iter().chain([&s.observer]) {
        s.sim.sent(p);
    }
}

/// Both `dsa`s, the tick, both `dsi`s and the first `dss`: the session is signing, waiting for
/// the second participant's signature.
fn to_last_signature(s: &mut Session<NodeMempool>, deliver: bool) -> Transaction {
    for tag in 1..=2 {
        dsa(s, tag, deliver);
        settle(s, |s| s.server.shared().lock().session().collateral_count() == usize::from(tag));
    }
    assert_eq!(s.server.info().state, PoolState::Queue);
    join(tick_on_scheduler(&s.server));
    assert_eq!(s.server.info().state, PoolState::AcceptingEntries);
    for tag in 1..=2 {
        dsi(s, tag, deliver);
        settle(s, |s| s.server.info().entries_count == usize::from(tag));
    }
    settle(s, |s| s.server.info().state == PoolState::Signing);
    let final_tx = s.server.shared().lock().session().final_tx().clone();
    dss(s, 1, deliver, &final_tx);
    settle(s, |s| signed(s, 1));
    drain(s);
    final_tx
}

fn nothing_sent(s: &Session<NodeMempool>) {
    for p in s.participants.iter().chain([&s.observer]) {
        assert_eq!(split(&s.sim, p), (vec![], vec![]));
    }
}

/// After a dropped `dsa`: no frame went to anyone, no session was opened, nothing was admitted,
/// and the next `dsa` opens the session.
fn after_a_dropped_dsa(node: OverNode, mut s: Session<NodeMempool>, deliver: bool) {
    nothing_sent(&s);
    let info = s.server.info();
    assert_eq!((info.state, info.queue_size, info.entries_count), (PoolState::Idle, 0, 0));
    assert_eq!(node.pool.admitted(), []);
    *node.pool.test_accepts.lock().unwrap() = None;
    dsa(&mut s, 1, deliver);
    settle(&mut s, |s| s.server.info().state == PoolState::Queue);
    assert_eq!(frames(&s.sim, &s.participants[0]).len(), 1);
    node.stop();
}

/// After a dropped final commit: no frame went to anyone, the session still waits for the last
/// signature, no collateral was charged, and the same `dss` then completes the session.
fn after_a_dropped_commit(
    node: OverNode,
    mut s: Session<NodeMempool>,
    deliver: bool,
    before: ServerInfo,
    final_tx: &Transaction,
) {
    nothing_sent(&s);
    assert_eq!(s.server.info(), before);
    assert!(!signed(&s, 2));
    let admitted = node.pool.admitted();
    assert_eq!(admitted.len(), 1, "only the final transaction's commit: {admitted:?}");
    assert!(![collateral(1), collateral(2)].iter().any(|c| admitted.contains(&txid(c))));
    *node.pool.admissions.lock().unwrap() = None;
    dss(&mut s, 2, deliver, final_tx);
    settle(&mut s, |s| s.server.info().state == PoolState::Idle);
    let dsc = (DSC, encode(&DsComplete::new(42, PoolMessage::MsgSuccess as i32)));
    for p in &s.participants {
        assert_eq!(frames(&s.sim, p), std::slice::from_ref(&dsc));
    }
    node.stop();
}

/// A `dsa` whose collateral test-accept the pool cannot answer, awaited outside the region on the
/// server's test-accept thread: one line, and the `dsa` is as if it never came.
#[test]
fn an_unavailable_collateral_test_accept_on_the_accept_thread_drops_the_dsa() {
    const TEXT: &str = "accept-thread test-accept unavailable";
    let node = OverNode::new();
    let mut s = node.session();
    *node.pool.test_accepts.lock().unwrap() = Some(TEXT);
    let from = service(&s.participants[0]);
    dsa(&mut s, 1, true);
    assert_eq!(node.await_line(&mut s, TEXT), [dropped(&format!("dsa from {from}"), TEXT)]);
    after_a_dropped_dsa(node, s, true);
}

/// The same `dsa` through the server's blocking call, whose test-accept is the mempool's own.
#[test]
fn an_unavailable_collateral_test_accept_in_a_blocking_call_drops_the_dsa() {
    const TEXT: &str = "blocking test-accept unavailable";
    let node = OverNode::new();
    let mut s = node.session();
    *node.pool.test_accepts.lock().unwrap() = Some(TEXT);
    let from = service(&s.participants[0]);
    dsa(&mut s, 1, false);
    assert_eq!(node.await_line(&mut s, TEXT), [dropped(&format!("dsa from {from}"), TEXT)]);
    after_a_dropped_dsa(node, s, false);
}

/// The last signature through the server's blocking call, whose final commit the pool cannot
/// admit: one line, and the `dss` is as if it never came.
#[test]
fn an_unavailable_final_commit_in_a_blocking_call_drops_the_last_dss() {
    const TEXT: &str = "blocking admission unavailable";
    let node = OverNode::new();
    let mut s = node.session();
    let final_tx = to_last_signature(&mut s, false);
    let before = s.server.info();
    *node.pool.admissions.lock().unwrap() = Some(TEXT);
    let from = service(&s.participants[1]);
    dss(&mut s, 2, false, &final_tx);
    assert_eq!(node.await_line(&mut s, TEXT), [dropped(&format!("dss from {from}"), TEXT)]);
    after_a_dropped_commit(node, s, false, before, &final_tx);
}

/// The last signature through the region, whose final commit the pool cannot admit, awaited
/// outside it: one line, the commit no longer in flight, and the `dss` is as if it never came.
#[test]
fn an_unavailable_final_commit_awaited_outside_the_region_drops_the_last_dss() {
    const TEXT: &str = "awaited admission unavailable";
    let node = OverNode::new();
    let mut s = node.session();
    let final_tx = to_last_signature(&mut s, true);
    let before = s.server.info();
    *node.pool.admissions.lock().unwrap() = Some(TEXT);
    let from = service(&s.participants[1]);
    dss(&mut s, 2, true, &final_tx);
    assert_eq!(node.await_line(&mut s, TEXT), [dropped(&format!("dss from {from}"), TEXT)]);
    after_a_dropped_commit(node, s, true, before, &final_tx);
}
