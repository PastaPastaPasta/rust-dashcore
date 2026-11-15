//! The masternode CoinJoin server runtime.

use dash_coinjoin::collateral::{MempoolAccept, test_accept_if_sane_fee};
use dash_coinjoin::denom::COIN;
use dash_coinjoin::dstx::{PoolLimits, txid};
use dash_coinjoin::inouts::{CoinView, ViewCoin};
use dash_coinjoin::pool::{PoolMessage, PoolState, PoolStatusUpdate};
use dash_coinjoin::queue::{DsqOutcome, MixingCounters, MixingMeta};
use dash_coinjoin::server::{ServerEffect, ServerEnv, ServerSession, SignedInputs};
use dash_p2p::policy::Throw;
use dash_p2p::{CommandString, OutMsg, Service};
use dash_p2p_core::{
    DsAccept, DsEntry, DsQueue, DsSignedInputs, DsStatusUpdate, InvType, Inventory,
};
use dash_primitives::{Transaction, TxHash};
use dash_types::CompactSize;
use dash_types::codec::BaseCodec;
use dash_types::{Hashable, Numeric};
use dashd_p2p::processing::logic::out_msg;
use dashd_p2p::processing::registry::CoreHandler;
use dashd_p2p::{HandlerCx, NetHandler};

use crate::dstx::DstxRuntime;
use crate::env::{ActiveMn, ChainView, Mempool, MnLists, PeerSink};

const DSA: CommandString = CommandString::from_static("dsa");
const DSI: CommandString = CommandString::from_static("dsi");
const DSS: CommandString = CommandString::from_static("dss");
const DSQ: CommandString = CommandString::from_static("dsq");
const DSSU: CommandString = CommandString::from_static("dssu");
const DSF: CommandString = CommandString::from_static("dsf");
const DSC: CommandString = CommandString::from_static("dsc");
// Core v24.0.0: coinjoin/server.cpp:481, the final-transaction priority bump.
const FINAL_TX_PRIORITY_DELTA: i64 = COIN / 10;

/// The clock used by the server (Core's seconds units).
pub trait ServerClock {
    /// `GetTime()`.
    fn now(&self) -> i64;
    /// `GetAdjustedTime()`.
    fn adjusted_time(&self) -> i64;
}

/// Injectable source for Core's `GetRand<int>` calls.
pub trait RandomSource {
    /// Draw below `upper`.
    fn below(&mut self, upper: u32) -> u32;
}

/// Host-backed implementation of the pure session environment.
struct RuntimeEnv<'a, C, M, A, P, S, Cl, R> {
    chain: &'a C,
    mns: &'a M,
    active: &'a mut A,
    mempool: &'a mut P,
    peers: &'a S,
    dstx: &'a DstxRuntime,
    clock: &'a Cl,
    random: &'a mut R,
}

impl<C, M, A, P, S, Cl, R> ServerEnv for RuntimeEnv<'_, C, M, A, P, S, Cl, R>
where
    C: ChainView,
    M: MnLists,
    A: ActiveMn,
    P: Mempool,
    S: PeerSink,
    Cl: ServerClock,
    R: RandomSource,
{
    type Peer = Service;
    fn now(&self) -> i64 {
        self.clock.now()
    }
    fn adjusted_time(&self) -> i64 {
        self.clock.adjusted_time()
    }
    fn rand_below(&mut self, n: u32) -> u32 {
        self.random.below(n)
    }
    fn v24_active_at_tip(&self) -> bool {
        self.chain.v24_active_at_tip()
    }
    fn valid_self(&self) -> Option<TxHash> {
        self.mns.valid_by_collateral(&self.active.outpoint()).map(|mn| mn.pro_tx_hash)
    }
    fn self_outpoint(&self) -> dash_primitives::OutPoint {
        self.active.outpoint()
    }
    fn self_pro_tx_hash(&self) -> TxHash {
        self.active.pro_tx_hash()
    }
    fn enabled_count(&self) -> i32 {
        self.mns.enabled_count()
    }
    fn sign_basic(&mut self, hash: &dash_num::Hash256) -> Vec<u8> {
        self.active.sign_basic(hash)
    }
    fn is_connected(&self, peer: &Service) -> bool {
        self.peers.is_connected(peer)
    }
    fn commit_to_mempool(&mut self, tx: &Transaction, txid: &TxHash) -> bool {
        self.mempool.prioritise(txid, FINAL_TX_PRIORITY_DELTA);
        test_accept_if_sane_fee(&*self.mempool, tx) && self.mempool.submit(tx)
    }
    fn has_dstx(&self, txid: &TxHash) -> bool {
        self.dstx.contains(txid)
    }
}

impl<C, M, A, P, S, Cl, R> MempoolAccept for RuntimeEnv<'_, C, M, A, P, S, Cl, R>
where
    P: Mempool,
{
    fn test_accept(&self, tx: &Transaction) -> Option<i64> {
        self.mempool.test_accept(tx)
    }
}

impl<C, M, A, P, S, Cl, R> CoinView for RuntimeEnv<'_, C, M, A, P, S, Cl, R>
where
    P: Mempool,
{
    fn coin(&self, outpoint: &dash_primitives::OutPoint) -> Option<ViewCoin> {
        self.mempool.coin(outpoint)
    }
    fn is_instantsend_locked(&self, txid: &TxHash) -> bool {
        self.mempool.is_instantsend_locked(txid)
    }
}

/// Runtime owner of a masternode's CoinJoin session.
pub struct CoinJoinServer<C, M, A, P, S, Cl, R, Meta = MixingCounters> {
    session: ServerSession<Service>,
    chain: C,
    mns: M,
    active: A,
    mempool: P,
    peers: S,
    clock: Cl,
    random: R,
    meta: Meta,
    dstx: DstxRuntime,
    /// Whether this runtime is installed on a masternode.
    masternode: bool,
}

/// The server half of `getcoinjoininfo`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerInfo {
    pub queue_size: usize,
    pub denomination: i64,
    pub state: PoolState,
    pub entries_count: usize,
}

impl<C, M, A, P, S, Cl, R, Meta> CoinJoinServer<C, M, A, P, S, Cl, R, Meta>
where
    C: ChainView,
    M: MnLists,
    A: ActiveMn,
    P: Mempool,
    S: PeerSink,
    Cl: ServerClock,
    R: RandomSource,
    Meta: MixingMeta,
{
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        limits: PoolLimits,
        chain: C,
        mns: M,
        active: A,
        mempool: P,
        peers: S,
        clock: Cl,
        random: R,
        meta: Meta,
        dstx: DstxRuntime,
    ) -> Self {
        Self {
            session: ServerSession::new(limits),
            chain,
            mns,
            active,
            mempool,
            peers,
            clock,
            random,
            meta,
            dstx,
            masternode: true,
        }
    }

    fn status_msg(&self, message: PoolMessage) -> OutMsg {
        out_msg(
            DSSU,
            &DsStatusUpdate::new(
                self.session.session_id(),
                self.session.state() as i32,
                PoolStatusUpdate::Rejected as i32,
                message as i32,
            ),
        )
    }

    fn run_effects(&mut self, cx: Option<&HandlerCx<'_>>, effects: Vec<ServerEffect<Service>>) {
        // Core server.cpp:1311-1320: `is_connected` is the chosen disconnected-participant signal.
        for effect in effects {
            match effect {
                ServerEffect::Status {
                    peer,
                    update,
                } => {
                    let msg = out_msg(DSSU, &update);
                    if let Some(c) = cx.filter(|c| c.peer.addr().service == peer) {
                        c.push(msg);
                    } else {
                        self.peers.push_to_addr(&peer, msg);
                    }
                }
                ServerEffect::FinalTx {
                    peer,
                    session_id,
                    tx,
                } => {
                    self.peers.push_to_addr(
                        &peer,
                        out_msg(DSF, &dash_p2p_core::DsFinalTx::new(session_id, tx)),
                    );
                }
                ServerEffect::Complete {
                    peer,
                    session_id,
                    message,
                } => {
                    self.peers.push_to_addr(
                        &peer,
                        out_msg(DSC, &dash_p2p_core::DsComplete::new(session_id, message as i32)),
                    );
                }
                ServerEffect::RelayQueue(queue) => self.peers.relay_dsq(&queue),
                ServerEffect::ConsumeCollateral(tx) => {
                    if test_accept_if_sane_fee(&self.mempool, &tx) && self.mempool.submit(&tx) {
                        self.peers.relay_transaction(&txid(&tx));
                    }
                }
                ServerEffect::AddDstx {
                    dstx,
                    ..
                } => self.dstx.on_accepted(dstx),
                ServerEffect::RelayDstxInv(hash) => {
                    self.peers.relay_tx_inv(Inventory::new(InvType::Dstx, hash.into()))
                }
            }
        }
    }

    /// Applies effects returned by the pure session in their Core order.
    pub fn execute_effects(&mut self, effects: Vec<ServerEffect<Service>>) {
        self.run_effects(None, effects);
    }

    pub fn session(&self) -> &ServerSession<Service> {
        &self.session
    }

    pub fn session_mut(&mut self) -> &mut ServerSession<Service> {
        &mut self.session
    }

    pub fn chain(&self) -> &C {
        &self.chain
    }

    pub fn mns(&self) -> &M {
        &self.mns
    }

    pub fn mns_mut(&mut self) -> &mut M {
        &mut self.mns
    }

    pub fn active(&self) -> &A {
        &self.active
    }

    pub fn mempool(&self) -> &P {
        &self.mempool
    }

    pub fn mempool_mut(&mut self) -> &mut P {
        &mut self.mempool
    }

    pub fn peers(&self) -> &S {
        &self.peers
    }

    pub fn peers_mut(&mut self) -> &mut S {
        &mut self.peers
    }

    pub fn clock(&self) -> &Cl {
        &self.clock
    }

    pub fn random(&self) -> &R {
        &self.random
    }

    pub fn random_mut(&mut self) -> &mut R {
        &mut self.random
    }

    pub fn meta(&self) -> &Meta {
        &self.meta
    }

    pub fn meta_mut(&mut self) -> &mut Meta {
        &mut self.meta
    }

    pub fn dstx(&self) -> &DstxRuntime {
        &self.dstx
    }

    pub fn masternode(&self) -> bool {
        self.masternode
    }

    pub fn set_masternode(&mut self, masternode: bool) {
        self.masternode = masternode;
    }

    pub fn process_dsq(&mut self, queue: DsQueue) -> DsqOutcome {
        self.session.process_dsq(&mut self.meta, &self.mns, queue, self.clock.adjusted_time())
    }

    /// Performs Core's final-transaction mempool admission sequence against the host mempool.
    pub fn commit_to_mempool(&mut self, tx: &Transaction) -> bool {
        let hash = txid(tx);
        let (mempool, dstx) = (&mut self.mempool, &self.dstx);
        let mut env = RuntimeEnv {
            chain: &self.chain,
            mns: &self.mns,
            active: &mut self.active,
            mempool,
            peers: &self.peers,
            dstx,
            clock: &self.clock,
            random: &mut self.random,
        };
        env.commit_to_mempool(tx, &hash)
    }

    /// Processes a server accept outside the p2p handler, for scheduler or host integrations.
    pub fn process_dsa(
        &mut self,
        peer: Service,
        version: dash_p2p::ProtocolVersion,
        dsa: &DsAccept,
    ) {
        let effects = {
            let (session, chain, mns, active, mempool, peers, dstx, clock, random) = (
                &mut self.session,
                &self.chain,
                &self.mns,
                &mut self.active,
                &mut self.mempool,
                &self.peers,
                &self.dstx,
                &self.clock,
                &mut self.random,
            );
            let mut env = RuntimeEnv {
                chain,
                mns,
                active,
                mempool,
                peers,
                dstx,
                clock,
                random,
            };
            session.process_dsa(&mut env, &self.meta, &peer, version, dsa)
        };
        self.run_effects(None, effects);
    }

    /// Processes an entry outside the p2p handler.
    pub fn process_dsi(&mut self, peer: Service, entry: DsEntry) {
        let effects = {
            let (session, chain, mns, active, mempool, peers, dstx, clock, random) = (
                &mut self.session,
                &self.chain,
                &self.mns,
                &mut self.active,
                &mut self.mempool,
                &self.peers,
                &self.dstx,
                &self.clock,
                &mut self.random,
            );
            let mut env = RuntimeEnv {
                chain,
                mns,
                active,
                mempool,
                peers,
                dstx,
                clock,
                random,
            };
            session.process_dsi(&mut env, &peer, entry)
        };
        self.run_effects(None, effects);
    }

    /// Processes signed final-transaction inputs outside the p2p handler.
    pub fn process_dss(&mut self, peer: Service, inputs: SignedInputs) {
        let effects = {
            let (session, chain, mns, active, mempool, peers, dstx, clock, random) = (
                &mut self.session,
                &self.chain,
                &self.mns,
                &mut self.active,
                &mut self.mempool,
                &self.peers,
                &self.dstx,
                &self.clock,
                &mut self.random,
            );
            let mut env = RuntimeEnv {
                chain,
                mns,
                active,
                mempool,
                peers,
                dstx,
                clock,
                random,
            };
            session.process_dss(&mut env, &peer, inputs)
        };
        self.run_effects(None, effects);
    }

    /// Core's once-per-second scheduler body, with sync and shutdown gates.
    pub fn tick(&mut self, shutdown_requested: bool) {
        if !self.chain.blockchain_synced() || shutdown_requested {
            return;
        }
        let effects = {
            let (session, chain, mns, active, mempool, peers, dstx, clock, random) = (
                &mut self.session,
                &self.chain,
                &self.mns,
                &mut self.active,
                &mut self.mempool,
                &self.peers,
                &self.dstx,
                &self.clock,
                &mut self.random,
            );
            let mut env = RuntimeEnv {
                chain,
                mns,
                active,
                mempool,
                peers,
                dstx,
                clock,
                random,
            };
            session.tick(&mut env)
        };
        self.run_effects(None, effects);
    }

    /// Server data for `getcoinjoininfo`.
    pub fn info(&self) -> ServerInfo {
        ServerInfo {
            queue_size: self.session.queues().len(),
            denomination: dash_coinjoin::denom::denomination_to_amount(
                self.session.session_denom(),
            ),
            state: self.session.state(),
            entries_count: self.session.entries().len(),
        }
    }
}

impl<C, M, A, P, S, Cl, R, Meta> NetHandler for CoinJoinServer<C, M, A, P, S, Cl, R, Meta>
where
    C: ChainView + Send,
    M: MnLists + Send,
    A: ActiveMn + Send,
    P: Mempool + Send,
    S: PeerSink + Send,
    Cl: ServerClock + Send,
    R: RandomSource + Send,
    Meta: MixingMeta + Send,
{
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
        if !self.chain.blockchain_synced() {
            return Ok(());
        }
        if !self.masternode {
            return Ok(());
        }
        let peer = cx.peer.addr().service.clone();
        let now = self.clock.now();
        match *cmd {
            DSA => {
                if let Some(reply) = self.session.pre_decode_dsa_reply(now) {
                    cx.push(self.status_msg(reply));
                    return Ok(());
                }
                let mut data = payload;
                let dsa: DsAccept = cx.read(&mut data)?;
                let effects = {
                    let (session, chain, mns, active, mempool, peers, dstx, clock, random) = (
                        &mut self.session,
                        &self.chain,
                        &self.mns,
                        &mut self.active,
                        &mut self.mempool,
                        &self.peers,
                        &self.dstx,
                        &self.clock,
                        &mut self.random,
                    );
                    let mut env = RuntimeEnv {
                        chain,
                        mns,
                        active,
                        mempool,
                        peers,
                        dstx,
                        clock,
                        random,
                    };
                    session.process_dsa(&mut env, &self.meta, &peer, cx.peer.common_version(), &dsa)
                };
                self.run_effects(Some(cx), effects);
            }
            DSI => {
                if let Some(reply) = self.session.pre_decode_dsi_reply(now) {
                    cx.push(self.status_msg(reply));
                    return Ok(());
                }
                let mut data = payload;
                let entry: DsEntry = cx.read(&mut data)?;
                let effects = {
                    let (session, chain, mns, active, mempool, peers, dstx, clock, random) = (
                        &mut self.session,
                        &self.chain,
                        &self.mns,
                        &mut self.active,
                        &mut self.mempool,
                        &self.peers,
                        &self.dstx,
                        &self.clock,
                        &mut self.random,
                    );
                    let mut env = RuntimeEnv {
                        chain,
                        mns,
                        active,
                        mempool,
                        peers,
                        dstx,
                        clock,
                        random,
                    };
                    session.process_dsi(&mut env, &peer, entry)
                };
                self.run_effects(Some(cx), effects);
            }
            DSS => {
                if let Some(reply) = self.session.pre_decode_dss_state_reply() {
                    cx.push(self.status_msg(reply));
                    return Ok(());
                }
                if let Some(reply) = self.session.pre_decode_dss_participant_reply(&peer) {
                    cx.push(self.status_msg(reply));
                    return Ok(());
                }
                let mut count_data = payload;
                let count = CompactSize::decode(&mut count_data)
                    .map_err(|e| Throw::from_decode(&dash_p2p_core::P2pDecodeError::Consensus(e)))?
                    .to_base();
                let count = usize::try_from(count).unwrap_or(usize::MAX);
                if let Some(reply) = self.session.pre_decode_dss_count_reply(count) {
                    cx.push(self.status_msg(reply));
                    return Ok(());
                }
                let mut data = payload;
                let signed: DsSignedInputs = cx.read(&mut data)?;
                let effects = {
                    let (session, chain, mns, active, mempool, peers, dstx, clock, random) = (
                        &mut self.session,
                        &self.chain,
                        &self.mns,
                        &mut self.active,
                        &mut self.mempool,
                        &self.peers,
                        &self.dstx,
                        &self.clock,
                        &mut self.random,
                    );
                    let mut env = RuntimeEnv {
                        chain,
                        mns,
                        active,
                        mempool,
                        peers,
                        dstx,
                        clock,
                        random,
                    };
                    session.process_dss(
                        &mut env,
                        &peer,
                        SignedInputs::Inputs(signed.inputs().clone()),
                    )
                };
                self.run_effects(Some(cx), effects);
            }
            DSQ => {
                let mut data = payload;
                let queue: DsQueue = cx.read(&mut data)?;
                let inv = Inventory::new(InvType::Dsq, queue.hash());
                cx.received_response(inv);
                let outcome = self.session.process_dsq(
                    &mut self.meta,
                    &self.mns,
                    queue,
                    self.clock.adjusted_time(),
                );
                if let DsqOutcome::Misbehaving(score) = outcome {
                    cx.misbehaving(score, "invalid CoinJoin queue");
                }
                if let DsqOutcome::Relay(hash) = outcome {
                    self.peers.forget_object_request(inv);
                    if let Some(q) = self.session.queues().queue_from_hash(&hash) {
                        self.peers.relay_dsq(&q.dsq);
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn already_have(&self, inv: &Inventory) -> bool {
        self.masternode
            && inv.inv_type() == InvType::Dsq
            && self.session.queues().has_queue(&inv.inv_hash())
    }

    fn process_getdata(&mut self, cx: &mut HandlerCx<'_>, inv: &Inventory) -> bool {
        if !self.masternode || inv.inv_type() != InvType::Dsq {
            return false;
        }
        let Some(queue) = self.session.queues().queue_from_hash(&inv.inv_hash()) else {
            return false;
        };
        cx.push(out_msg(DSQ, &queue.dsq));
        true
    }
}
