//! The masternode CoinJoin server runtime.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::future::{Future, ready};
use std::pin::{Pin, pin};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Wake, Waker};

use dash_coinjoin::collateral::{MAX_RAW_TX_FEE, MempoolAccept, test_accept_if_sane_fee};
use dash_coinjoin::denom::{COIN, PROMOTION_RATIO};
use dash_coinjoin::dstx::{PoolLimits, txid};
use dash_coinjoin::inouts::{CoinView, ViewCoin};
use dash_coinjoin::pool::{ENTRY_MAX_SIZE, PoolMessage, PoolState, PoolStatusUpdate};
use dash_coinjoin::queue::{DsqOutcome, MixingCounters, MixingMeta};
use dash_coinjoin::server::{ServerEffect, ServerEnv, ServerSession, SignedInputs};
use dash_p2p::policy::Throw;
use dash_p2p::{CommandString, OutMsg, PeerHandle, ProtocolVersion, Service};
use dash_p2p_core::{
    DsAccept, DsEntry, DsQueue, DsSignedInputs, DsStatusUpdate, InvType, Inventory,
};
use dash_primitives::{OutPoint, Transaction, TxHash, TxIn};
use dash_types::CompactSize;
use dash_types::codec::BaseCodec;
use dash_types::{Hashable, Numeric};
use dashd_p2p::processing::logic::out_msg;
use dashd_p2p::processing::registry::{CoreHandler, HandlerResult, PostRegion};
use dashd_p2p::processing::seams::Interrupted;
use dashd_p2p::{HandlerCx, NetHandler, PeerLogic};

use crate::dstx::DstxRuntime;
use crate::env::{ActiveMn, AsyncMempool, ChainView, Mempool, MnLists, PeerSink, Pending};

const DSA: CommandString = CommandString::from_static("dsa");
const DSI: CommandString = CommandString::from_static("dsi");
const DSS: CommandString = CommandString::from_static("dss");
const DSQ: CommandString = CommandString::from_static("dsq");
const DSSU: CommandString = CommandString::from_static("dssu");
const DSF: CommandString = CommandString::from_static("dsf");
const DSC: CommandString = CommandString::from_static("dsc");
// Core v24.0.0: coinjoin/server.cpp:481, the final-transaction priority bump.
const FINAL_TX_PRIORITY_DELTA: i64 = COIN / 10;
/// The most runs of one message. A `dsa` or `dsi` needs one, its reads answered before it; a
/// `dss` that completes the signatures needs two, the final transaction's commit answered between
/// them. The last run refuses whatever it still misses.
const MAX_RUNS: usize = 2;
/// The most entry inputs whose coins are read ahead: no session takes a larger entry (the
/// standard cap, or a promotion's inputs).
const ENTRY_READS: usize = PROMOTION_RATIO as usize;
const _: () = assert!(ENTRY_READS >= ENTRY_MAX_SIZE);

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
    answered: Option<&'a mut Answered>,
}

/// The mempool and coins answers a continuation awaited for one message, and the clock and draws
/// its runs share, so that each rerun takes the path the last one took.
#[derive(Default)]
struct Answered {
    coins: BTreeMap<OutPoint, Option<ViewCoin>>,
    locks: BTreeMap<TxHash, bool>,
    accepts: BTreeMap<TxHash, Option<i64>>,
    commits: BTreeMap<TxHash, bool>,
    draws: Vec<u32>,
    drawn: usize,
    now: i64,
    adjusted: i64,
    missing: RefCell<Missing>,
}

/// What a run asked for that its [`Answered`] did not have.
#[derive(Default)]
struct Missing {
    coins: Vec<OutPoint>,
    locks: Vec<TxHash>,
    accepts: Vec<Transaction>,
    commits: Vec<Transaction>,
}

impl Missing {
    fn is_empty(&self) -> bool {
        self.coins.is_empty()
            && self.locks.is_empty()
            && self.accepts.is_empty()
            && self.commits.is_empty()
    }
}

impl Answered {
    fn coin(&self, outpoint: &OutPoint) -> Option<ViewCoin> {
        let coin = self.coins.get(outpoint);
        if coin.is_none() {
            self.missing.borrow_mut().coins.push(*outpoint);
        }
        coin.cloned().flatten()
    }

    fn is_locked(&self, txid: &TxHash) -> bool {
        let locked = self.locks.get(txid);
        if locked.is_none() {
            self.missing.borrow_mut().locks.push(*txid);
        }
        locked.copied().unwrap_or(false)
    }

    /// Whether `outpoint`'s coin is known, unspent, and confirmed or InstantSend-locked.
    fn usable(&self, outpoint: &OutPoint) -> bool {
        let coin = self.coins.get(outpoint).and_then(Option::as_ref);
        coin.is_some_and(|c| !c.in_mempool || self.locks.get(&outpoint.txid()) == Some(&true))
    }

    fn test_accept(&self, tx: &Transaction) -> Option<i64> {
        let fee = self.accepts.get(&txid(tx));
        if fee.is_none() {
            self.missing.borrow_mut().accepts.push(tx.clone());
        }
        fee.copied().flatten()
    }

    fn commit(&self, tx: &Transaction, txid: &TxHash) -> bool {
        let committed = self.commits.get(txid);
        if committed.is_none() {
            self.missing.borrow_mut().commits.push(tx.clone());
        }
        committed.copied().unwrap_or(false)
    }

    /// Every read of a `dsa`'s or `dsi`'s run, before it: the coins of the collateral's and the
    /// entry's inputs in one call, the InstantSend locks of those in the mempool, then the
    /// collateral's test-accept when its coins are usable. `None` when the node stopped.
    // Core: coinjoin/coinjoin.cpp:476-497 (IsCollateralValid), :376-393 (IsValidInOuts)
    async fn precompute(&mut self, admit: &Admit, mempool: &dyn AsyncMempool) -> Option<()> {
        let Some((collateral, inputs)) = admit.reads() else {
            return Some(());
        };
        let missing = self.missing.get_mut();
        missing.coins = collateral.inputs().iter().chain(inputs).map(TxIn::prevout).collect();
        self.fetch(mempool).await?;
        if collateral.inputs().iter().all(|i| self.usable(&i.prevout())) {
            self.missing.get_mut().accepts.push(collateral.clone());
            self.fetch(mempool).await?;
        }
        Some(())
    }

    /// Awaits the missing coins, the locks of mempool coins and the missing test-accepts.
    /// `None` when the node stopped.
    async fn fetch(&mut self, mempool: &dyn AsyncMempool) -> Option<()> {
        let missing = self.missing.take();
        if !missing.coins.is_empty() {
            let coins = mempool.coins(missing.coins.clone()).await?;
            self.coins.extend(missing.coins.into_iter().zip(coins));
        }
        let pooled = self.coins.iter().filter(|(_, c)| c.as_ref().is_some_and(|c| c.in_mempool));
        let mut txids: Vec<TxHash> = pooled.map(|(o, _)| o.txid()).chain(missing.locks).collect();
        txids.sort();
        txids.dedup();
        txids.retain(|t| !self.locks.contains_key(t));
        if !txids.is_empty() {
            let locked = mempool.instantsend_locked(txids.clone()).await?;
            self.locks.extend(txids.into_iter().zip(locked));
        }
        for tx in missing.accepts {
            let fee = mempool.test_accept(tx.clone()).await?;
            self.accepts.insert(txid(&tx), fee);
        }
        Some(())
    }
}

/// `ATMPIfSaneFee(tx, false)`: the test-accept, the fee cap, then the submission.
// Core: coinjoin/coinjoin.cpp:440-456
async fn submit_if_sane_fee(mempool: &dyn AsyncMempool, tx: Transaction) -> Option<bool> {
    let fee = mempool.test_accept(tx.clone()).await?;
    if !fee.is_some_and(|fee| fee <= MAX_RAW_TX_FEE) {
        return Some(false);
    }
    mempool.submit(tx).await
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
        self.answered.as_ref().map_or_else(|| self.clock.now(), |a| a.now)
    }
    fn adjusted_time(&self) -> i64 {
        self.answered.as_ref().map_or_else(|| self.clock.adjusted_time(), |a| a.adjusted)
    }
    fn rand_below(&mut self, n: u32) -> u32 {
        let Some(a) = self.answered.as_deref_mut() else {
            return self.random.below(n);
        };
        if a.drawn == a.draws.len() {
            a.draws.push(self.random.below(n));
        }
        a.drawn += 1;
        a.draws[a.drawn - 1]
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
        if let Some(a) = &self.answered {
            return a.commit(tx, txid);
        }
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
        match &self.answered {
            Some(a) => a.test_accept(tx),
            None => self.mempool.test_accept(tx),
        }
    }
}

impl<C, M, A, P, S, Cl, R> CoinView for RuntimeEnv<'_, C, M, A, P, S, Cl, R>
where
    P: Mempool,
{
    fn coin(&self, outpoint: &OutPoint) -> Option<ViewCoin> {
        match &self.answered {
            Some(a) => a.coin(outpoint),
            None => self.mempool.coin(outpoint),
        }
    }
    fn is_instantsend_locked(&self, txid: &TxHash) -> bool {
        match &self.answered {
            Some(a) => a.is_locked(txid),
            None => self.mempool.is_instantsend_locked(txid),
        }
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
    dstx: Arc<DstxRuntime>,
    /// Whether this runtime is installed on a masternode.
    masternode: bool,
    /// The session whose final transaction's commit is in flight.
    committing: Option<i32>,
    /// Admission tails whose sender disconnected, for the next tick to finish.
    orphans: Arc<Mutex<Vec<Task>>>,
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
        dstx: Arc<DstxRuntime>,
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
            committing: None,
            orphans: Arc::default(),
        }
    }

    fn with_env<T>(
        &mut self,
        answered: Option<&mut Answered>,
        f: impl FnOnce(
            &mut ServerSession<Service>,
            &mut RuntimeEnv<'_, C, M, A, P, S, Cl, R>,
            &Meta,
        ) -> T,
    ) -> T {
        let mut env = RuntimeEnv {
            chain: &self.chain,
            mns: &self.mns,
            active: &mut self.active,
            mempool: &mut self.mempool,
            peers: &self.peers,
            dstx: &self.dstx,
            clock: &self.clock,
            random: &mut self.random,
            answered,
        };
        f(&mut self.session, &mut env, &self.meta)
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

    /// Applies `effects`, pushing a status for `cx`'s peer through `cx`. Each collateral to consume
    /// goes to `consumed` when given, otherwise through [`Self::consume`].
    fn run_effects(
        &mut self,
        cx: Option<&HandlerCx<'_>>,
        effects: Vec<ServerEffect<Service>>,
        mut consumed: Option<&mut Vec<Transaction>>,
    ) {
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
                ServerEffect::ConsumeCollateral(tx) => match consumed.as_deref_mut() {
                    Some(consumed) => consumed.push(tx),
                    None => self.consume(&tx),
                },
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

    /// `ConsumeCollateral`: `ATMPIfSaneFee`, and the relay only when the mempool took it.
    // Core: coinjoin/server.cpp:619-628
    fn consume(&mut self, tx: &Transaction) {
        if test_accept_if_sane_fee(&self.mempool, tx) && self.mempool.submit(tx) {
            self.peers.relay_transaction(&txid(tx));
        }
    }

    /// Applies effects returned by the pure session in their Core order.
    pub fn execute_effects(&mut self, effects: Vec<ServerEffect<Service>>) {
        self.run_effects(None, effects, None);
    }

    /// Runs `admit` with `answered`. Before the `last` run, a run that asks for the final
    /// transaction's commit restores the session and marks the commit in flight, and one that
    /// misses another answer restores it. Otherwise the run keeps the session it left and
    /// applies its effects, returning the collaterals to consume.
    fn try_admit(&mut self, admit: &Admit, answered: &mut Answered, last: bool) -> Run {
        answered.drawn = 0;
        let before = self.session.clone();
        let effects =
            self.with_env(Some(&mut *answered), |session, env, meta| admit.run(session, env, meta));
        let missing = answered.missing.get_mut();
        if !last && !missing.is_empty() {
            self.session = before;
            let commit = missing.commits.pop();
            debug_assert!(missing.is_empty(), "a precomputed read was missed");
            let Some(tx) = commit else {
                return Run::Missing;
            };
            debug_assert!(self.committing.is_none());
            self.committing = Some(self.session.session_id());
            answered.missing.take();
            return Run::Commit(tx);
        }
        debug_assert!(missing.is_empty(), "a message ran out of runs");
        answered.missing.take();
        let mut consumed = Vec::new();
        self.run_effects(None, effects, Some(&mut consumed));
        Run::Kept(consumed)
    }

    /// The `dss` run with its commit's answer, which ends the commit in flight.
    fn finish_commit(&mut self, admit: &Admit, answered: &mut Answered) -> Vec<Transaction> {
        self.committing = None;
        match self.try_admit(admit, answered, true) {
            Run::Kept(consumed) => consumed,
            Run::Missing | Run::Commit(_) => Vec::new(),
        }
    }

    /// The gates, the pre-decode replies and the read of a server message. `dsq` is handled
    /// here; a `dsa`, `dsi` or `dss` is returned for its admission.
    fn admission(
        &mut self,
        cx: &mut HandlerCx<'_>,
        cmd: &CommandString,
        payload: &[u8],
    ) -> Result<Option<Admit>, Throw> {
        if !self.chain.blockchain_synced() || !self.masternode {
            return Ok(None);
        }
        let peer = cx.peer.addr().service.clone();
        let now = self.clock.now();
        let reply = match *cmd {
            DSA => self.session.pre_decode_dsa_reply(now),
            DSI => self.session.pre_decode_dsi_reply(now),
            DSS => self
                .session
                .pre_decode_dss_state_reply()
                .or_else(|| self.session.pre_decode_dss_participant_reply(&peer)),
            _ => None,
        };
        if let Some(reply) = reply {
            cx.push(self.status_msg(reply));
            return Ok(None);
        }
        let mut data = payload;
        match *cmd {
            DSA => Ok(Some(Admit::Dsa(peer, cx.peer.common_version(), cx.read(&mut data)?))),
            DSI => Ok(Some(Admit::Dsi(peer, cx.read(&mut data)?))),
            DSS => {
                let mut count_data = payload;
                let count = CompactSize::decode(&mut count_data)
                    .map_err(|e| Throw::from_decode(&dash_p2p_core::P2pDecodeError::Consensus(e)))?
                    .to_base();
                let count = usize::try_from(count).unwrap_or(usize::MAX);
                if let Some(reply) = self.session.pre_decode_dss_count_reply(count) {
                    cx.push(self.status_msg(reply));
                    return Ok(None);
                }
                let signed: DsSignedInputs = cx.read(&mut data)?;
                // Core's CheckPool skips its round while the commit holds cs_check_pool.
                if self.committing.is_some() {
                    return Ok(None);
                }
                Ok(Some(Admit::Dss(peer, SignedInputs::Inputs(signed.inputs().clone()))))
            }
            DSQ => {
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
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    /// The tick's session work; its collaterals to consume go to `consumed` when given. While a
    /// commit is in flight it skips the session's checks, as Core's tick skips `CheckPool` and
    /// `CheckTimeout` while the commit holds `cs_check_pool` (its queue expiry waits too).
    // Core: coinjoin/server.cpp:314-321, :712-719
    fn tick_into(&mut self, shutdown_requested: bool, consumed: Option<&mut Vec<Transaction>>) {
        if !self.chain.blockchain_synced() || shutdown_requested || self.committing.is_some() {
            return;
        }
        let effects = self.with_env(None, |session, env, _| session.tick(env));
        self.run_effects(None, effects, consumed);
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

    pub fn dstx(&self) -> &Arc<DstxRuntime> {
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
        self.with_env(None, |_, env, _| env.commit_to_mempool(tx, &txid(tx)))
    }

    /// Processes a server accept outside the p2p handler, for scheduler or host integrations.
    pub fn process_dsa(
        &mut self,
        peer: Service,
        version: dash_p2p::ProtocolVersion,
        dsa: &DsAccept,
    ) {
        let effects = self.with_env(None, |session, env, meta| {
            session.process_dsa(env, meta, &peer, version, dsa)
        });
        self.run_effects(None, effects, None);
    }

    /// Processes an entry outside the p2p handler.
    pub fn process_dsi(&mut self, peer: Service, entry: DsEntry) {
        let effects = self.with_env(None, |session, env, _| session.process_dsi(env, &peer, entry));
        self.run_effects(None, effects, None);
    }

    /// Processes signed final-transaction inputs outside the p2p handler.
    pub fn process_dss(&mut self, peer: Service, inputs: SignedInputs) {
        let effects =
            self.with_env(None, |session, env, _| session.process_dss(env, &peer, inputs));
        self.run_effects(None, effects, None);
    }

    /// Core's once-per-second scheduler body, with sync and shutdown gates.
    pub fn tick(&mut self, shutdown_requested: bool) {
        self.tick_into(shutdown_requested, None);
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

    /// Runs the admission in the peer region, calling the mempool under the server's state: for
    /// a host's own use and the tests. The node registers [`SharedServer`], whose admission waits
    /// outside the region.
    fn process_message(
        &mut self,
        cx: &mut HandlerCx<'_>,
        cmd: &CommandString,
        payload: &[u8],
    ) -> Result<(), Throw> {
        if let Some(admit) = self.admission(cx, cmd, payload)? {
            let effects = self.with_env(None, |session, env, meta| admit.run(session, env, meta));
            self.run_effects(Some(cx), effects, None);
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

/// A cloneable handle that runs the P2P handler, the scheduler tick and `getcoinjoininfo` on one
/// shared [`CoinJoinServer`] session.
pub struct SharedServer<T>(Arc<Mutex<T>>);

impl<T> Clone for SharedServer<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T> SharedServer<T> {
    pub fn new(server: T) -> Self {
        Self(Arc::new(Mutex::new(server)))
    }

    pub fn lock(&self) -> MutexGuard<'_, T> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A decoded `dsa`, `dsi` or `dss` and its sender.
enum Admit {
    Dsa(Service, ProtocolVersion, DsAccept),
    Dsi(Service, DsEntry),
    Dss(Service, SignedInputs),
}

/// What one run of an [`Admit`] did.
enum Run {
    /// It kept its session; these collaterals are to be consumed.
    Kept(Vec<Transaction>),
    /// It asked for this final transaction's commit, now in flight.
    Commit(Transaction),
    /// It missed an answer.
    Missing,
}

impl Admit {
    /// The collateral and the entry inputs whose coins a run reads; none for a `dss`.
    fn reads(&self) -> Option<(&Transaction, &[TxIn])> {
        match self {
            Admit::Dsa(_, _, dsa) => Some((dsa.collateral(), &[])),
            Admit::Dsi(_, entry) => {
                let inputs = entry.inputs();
                Some((
                    entry.collateral(),
                    if inputs.len() <= ENTRY_READS {
                        inputs
                    } else {
                        &[]
                    },
                ))
            }
            Admit::Dss(..) => None,
        }
    }

    fn run<E: ServerEnv<Peer = Service>, M: MixingMeta + ?Sized>(
        &self,
        session: &mut ServerSession<Service>,
        env: &mut E,
        meta: &M,
    ) -> Vec<ServerEffect<Service>> {
        match self {
            Admit::Dsa(peer, version, dsa) => session.process_dsa(env, meta, peer, *version, dsa),
            Admit::Dsi(peer, entry) => session.process_dsi(env, peer, entry.clone()),
            Admit::Dss(peer, inputs) => session.process_dss(env, peer, inputs.clone()),
        }
    }
}

/// Work an admission issued and must finish.
type Task = Pin<Box<dyn Future<Output = ()> + Send>>;

/// A `dsa`/`dsi`/`dss` admission outside the peer region, awaiting the mempool never under the
/// server's lock. A `dsa`'s or `dsi`'s reads are awaited before its one run, which keeps its
/// session and applies its effects under one lock hold. A `dss` that completes the signatures
/// asks for the final transaction's commit: that run is undone and the commit marked in flight,
/// and the tail prioritises and submits the transaction, then reruns the `dss` with the answer
/// to apply the completion. The tail also submits the consumed collaterals, relaying each the
/// mempool takes.
///
/// Core v24.0.x also leaves `cs_coinjoin` for the mempool work. It keeps the session consistent
/// with `session_id` re-checks, a `dss`'s signatures applied in one critical section, and
/// `cs_check_pool`, which makes the commit and the timeout single-flight. Here the session
/// cannot move between a `dss`'s two runs: the tick skips its pool and timeout checks while a
/// commit is in flight and a later `dss` is dropped (`cs_check_pool`), and the signatures are
/// kept with the commit and applied with its answer. A dropped admission leaves the session as
/// if the message never came, until its tail starts; a started tail goes to the server, and the
/// next tick finishes it.
// Core: coinjoin/server.cpp:266-294, :314-321, :453-470, :712-719
struct Admission {
    stage: Stage,
    orphans: Arc<Mutex<Vec<Task>>>,
}

enum Stage {
    Runs(Pin<Box<dyn Future<Output = Option<Task>> + Send>>),
    Tail(Task),
    Done,
}

impl PostRegion for Admission {
    fn poll(
        &mut self,
        _: &mut PeerLogic,
        _: &PeerHandle,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), Interrupted>> {
        if let Stage::Runs(runs) = &mut self.stage {
            self.stage = match runs.as_mut().poll(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(tail)) => Stage::Tail(tail),
                Poll::Ready(None) => Stage::Done,
            };
        }
        if let Stage::Tail(tail) = &mut self.stage {
            if tail.as_mut().poll(cx).is_pending() {
                return Poll::Pending;
            }
            self.stage = Stage::Done;
        }
        Poll::Ready(Ok(()))
    }

    fn tail(self: Box<Self>, _: &mut HandlerCx<'_>) -> Result<(), Throw> {
        Ok(())
    }
}

impl Drop for Admission {
    fn drop(&mut self) {
        if let Stage::Tail(tail) = std::mem::replace(&mut self.stage, Stage::Done) {
            lock(&self.orphans).push(tail);
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// [`AsyncMempool`] over a mempool that never waits ([`Mempool::awaitable`] is `None`), each call
/// made under the server's lock.
struct Locked<T>(SharedServer<T>);

impl<C, M, A, P, S, Cl, R, Meta> AsyncMempool for Locked<CoinJoinServer<C, M, A, P, S, Cl, R, Meta>>
where
    C: Send,
    M: Send,
    A: Send,
    P: Mempool + Send,
    S: Send,
    Cl: Send,
    R: Send,
    Meta: Send,
{
    fn coins(&self, outpoints: Vec<OutPoint>) -> Pending<Vec<Option<ViewCoin>>> {
        let server = self.0.lock();
        Box::pin(ready(Some(outpoints.iter().map(|o| server.mempool.coin(o)).collect())))
    }

    fn instantsend_locked(&self, txids: Vec<TxHash>) -> Pending<Vec<bool>> {
        let server = self.0.lock();
        let locked = txids.iter().map(|t| server.mempool.is_instantsend_locked(t)).collect();
        Box::pin(ready(Some(locked)))
    }

    fn test_accept(&self, tx: Transaction) -> Pending<Option<i64>> {
        Box::pin(ready(Some(self.0.lock().mempool.test_accept(&tx))))
    }

    fn submit(&self, tx: Transaction) -> Pending<bool> {
        Box::pin(ready(Some(self.0.lock().mempool.submit(&tx))))
    }

    fn prioritise(&self, txid: TxHash, fee_delta: i64) -> Pending<()> {
        self.0.lock().mempool.prioritise(&txid, fee_delta);
        Box::pin(ready(Some(())))
    }
}

/// Polls `f` to completion on this thread.
fn block_on<F: Future>(f: F) -> F::Output {
    struct Unpark(std::thread::Thread);
    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    let mut f = pin!(f);
    loop {
        if let Poll::Ready(out) = f.as_mut().poll(&mut cx) {
            return out;
        }
        std::thread::park();
    }
}

impl<C, M, A, P, S, Cl, R, Meta> SharedServer<CoinJoinServer<C, M, A, P, S, Cl, R, Meta>>
where
    C: ChainView + Send + 'static,
    M: MnLists + Send + 'static,
    A: ActiveMn + Send + 'static,
    P: Mempool + Send + 'static,
    S: PeerSink + Send + 'static,
    Cl: ServerClock + Send + 'static,
    R: RandomSource + Send + 'static,
    Meta: MixingMeta + Send + 'static,
{
    /// [`CoinJoinServer::tick`] on the shared session. It first finishes the admission tails a
    /// disconnect left; then the collaterals it consumes are submitted after the lock is
    /// released. This thread may wait for both.
    pub fn tick(&self, shutdown_requested: bool) {
        let orphans = Arc::clone(&self.lock().orphans);
        let tails = std::mem::take(&mut *lock(&orphans));
        for tail in tails {
            block_on(tail);
        }
        let mut consumed = Vec::new();
        self.lock().tick_into(shutdown_requested, Some(&mut consumed));
        let mempool = self.awaitable();
        for tx in consumed {
            if block_on(submit_if_sane_fee(&*mempool, tx.clone())) == Some(true) {
                self.lock().peers.relay_transaction(&txid(&tx));
            }
        }
    }

    /// [`CoinJoinServer::info`] on the shared session.
    pub fn info(&self) -> ServerInfo {
        self.lock().info()
    }

    fn awaitable(&self) -> Arc<dyn AsyncMempool> {
        let mempool = self.lock().mempool.awaitable();
        mempool.unwrap_or_else(|| Arc::new(Locked(self.clone())))
    }

    /// The runs of `admit`, returning the tail that finishes what they issued. `None` when the
    /// node stopped first.
    async fn admit(self, admit: Admit, now: i64, adjusted: i64) -> Option<Task> {
        let mempool = self.awaitable();
        let mut answered = Answered {
            now,
            adjusted,
            ..Answered::default()
        };
        answered.precompute(&admit, &*mempool).await?;
        let mut run = 1;
        loop {
            let outcome = self.lock().try_admit(&admit, &mut answered, run == MAX_RUNS);
            match outcome {
                Run::Kept(consumed) => return Some(Box::pin(self.consume(mempool, consumed))),
                Run::Commit(tx) => {
                    return Some(Box::pin(self.commit(mempool, admit, answered, tx)));
                }
                Run::Missing => answered.fetch(&*mempool).await?,
            }
            run += 1;
        }
    }

    /// `CommitFinalTransaction`'s mempool half, then the `dss` run with its answer.
    // Core: coinjoin/server.cpp:477-489
    async fn commit(
        self,
        mempool: Arc<dyn AsyncMempool>,
        admit: Admit,
        mut answered: Answered,
        tx: Transaction,
    ) {
        let id = txid(&tx);
        let committed = match mempool.prioritise(id, FINAL_TX_PRIORITY_DELTA).await {
            Some(()) => submit_if_sane_fee(&*mempool, tx).await == Some(true),
            None => false,
        };
        answered.commits.insert(id, committed);
        let consumed = self.lock().finish_commit(&admit, &mut answered);
        self.consume(mempool, consumed).await;
    }

    /// `ConsumeCollateral` of each: the relay only when the mempool took it.
    // Core: coinjoin/server.cpp:619-628
    async fn consume(self, mempool: Arc<dyn AsyncMempool>, consumed: Vec<Transaction>) {
        for tx in consumed {
            if submit_if_sane_fee(&*mempool, tx.clone()).await == Some(true) {
                self.lock().peers.relay_transaction(&txid(&tx));
            }
        }
    }
}

impl<C, M, A, P, S, Cl, R, Meta> NetHandler
    for SharedServer<CoinJoinServer<C, M, A, P, S, Cl, R, Meta>>
where
    C: ChainView + Send + 'static,
    M: MnLists + Send + 'static,
    A: ActiveMn + Send + 'static,
    P: Mempool + Send + 'static,
    S: PeerSink + Send + 'static,
    Cl: ServerClock + Send + 'static,
    R: RandomSource + Send + 'static,
    Meta: MixingMeta + Send + 'static,
{
    fn commands(&self) -> &'static [CommandString] {
        self.lock().commands()
    }

    fn core_handler(&self) -> Option<CoreHandler> {
        self.lock().core_handler()
    }

    fn process_message(
        &mut self,
        _: &mut HandlerCx<'_>,
        _: &CommandString,
        _: &[u8],
    ) -> Result<(), Throw> {
        unreachable!("the registry calls process_message_result")
    }

    /// The gates, replies and read in the region; a `dsa`, `dsi` or `dss` continues outside it,
    /// waiting on the mempool.
    fn process_message_result(
        &mut self,
        cx: &mut HandlerCx<'_>,
        cmd: &CommandString,
        payload: &[u8],
    ) -> Result<HandlerResult, Throw> {
        let (admit, now, adjusted, orphans) = {
            let mut server = self.lock();
            let admit = server.admission(cx, cmd, payload)?;
            (admit, server.clock.now(), server.clock.adjusted_time(), Arc::clone(&server.orphans))
        };
        Ok(match admit {
            Some(admit) => HandlerResult::Continue(Box::new(Admission {
                stage: Stage::Runs(Box::pin(self.clone().admit(admit, now, adjusted))),
                orphans,
            })),
            None => HandlerResult::Done,
        })
    }

    fn already_have(&self, inv: &Inventory) -> bool {
        self.lock().already_have(inv)
    }

    fn process_getdata(&mut self, cx: &mut HandlerCx<'_>, inv: &Inventory) -> bool {
        self.lock().process_getdata(cx, inv)
    }
}
