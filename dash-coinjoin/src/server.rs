//! The mixing masternode's session follows Dash Core v24.0.0 (`CCoinJoinServer`): `dsa`
//! admission, `dsi` entries, the final transaction, `dss` signatures, fees, timeouts and resets.
//!
//! [`ServerSession`] holds Core's session state; each method is one of Core's handlers or
//! scheduler steps, run to completion (Core's interleavings between its message thread and its
//! scheduler, and the re-checks it makes for them, collapse: CJ0 notes D7). The node supplies
//! the clock, randomness, chain, mempool and signing key through [`ServerEnv`] and carries out the
//! returned [`ServerEffect`]s in order.

use std::collections::{BTreeMap, BTreeSet};

use dash_consensus::script::sighash::TxSigData;
use dash_consensus::script::{
    DirectVerifier, ScriptFlags, TransactionSignatureChecker, verify_script,
};
use dash_num::Hash256;
use dash_p2p_core::{DsAccept, DsEntry, DsQueue, DsStatusUpdate, DsTx, ProtocolVersion};
use dash_primitives::{OutPoint, Transaction, TxHash, TxIn, TxOut, TxType};

use crate::collateral::{MempoolAccept, is_collateral_valid};
use crate::denom::{self, MixShape, MixSideCounts};
use crate::dstx::PoolLimits;
use crate::inouts::{CoinView, InOutsKind, is_valid_in_outs, sort_bip69, with_in_outs};
use crate::pool::{
    AcceptFlags, ENTRY_MAX_SIZE, PoolMessage, PoolState, PoolStatusUpdate, state_timeout,
};
use crate::queue::{
    DsqOutcome, MixingMeta, QueueEntry, QueueManager, TipMasternodes, server_process_dsq,
};

/// What the masternode's session reads from and asks of the node.
pub trait ServerEnv: CoinView + MempoolAccept {
    /// `GetTime()`, seconds.
    fn now(&self) -> i64;

    /// `GetAdjustedTime()`, seconds: a new queue's and DSTX's time.
    fn adjusted_time(&self) -> i64;

    /// `GetRand<int>(n)`: uniform in `0..n`, for `n >= 1`. The session reduces every answer it
    /// uses as an index or an id modulo `n`, so an answer outside the range cannot panic or
    /// overflow; it only skews the draw.
    fn rand_below(&mut self, n: u32) -> u32;

    /// `IsPromotionDemotionActive()`: V24 active at the tip.
    fn v24_active_at_tip(&self) -> bool;

    /// This masternode's ProRegTx hash, when it is a valid masternode in the tip list
    /// (`GetValidMNByCollateral(GetOutPoint())`).
    fn valid_self(&self) -> Option<TxHash>;

    /// This masternode's collateral outpoint (`m_mn_activeman.GetOutPoint()`).
    fn self_outpoint(&self) -> OutPoint;

    /// This masternode's ProRegTx hash (`m_mn_activeman.GetProTxHash()`).
    fn self_pro_tx_hash(&self) -> TxHash;

    /// `GetListAtChainTip().GetCounts().enabled()`.
    fn enabled_count(&self) -> i32;

    /// `m_mn_activeman.SignBasic(hash)`: the operator's basic-scheme signature bytes.
    fn sign_basic(&mut self, hash: &Hash256) -> Vec<u8>;

    /// Whether the node is connected to `peer` (`connman.ForNode` finds it).
    fn is_connected(&self, peer: &Self::Peer) -> bool;

    /// The peers' identity (Core's `CService` of an entry).
    type Peer: Clone + Eq;

    /// The final transaction's commit: `PrioritiseTransaction(txid, 0.1 * COIN)`, then
    /// `ATMPIfSaneFee(tx)` (test-accept, the fee cap, then the submission). Whether the mempool
    /// took it.
    fn commit_to_mempool(&mut self, tx: &Transaction, txid: &TxHash) -> bool;

    /// Whether the node keeps a DSTX for `txid` (`m_dstxman.GetDSTX(txid)`).
    fn has_dstx(&self, txid: &TxHash) -> bool;
}

/// Something the node does for the session, in order.
#[derive(Clone, Debug)]
pub enum ServerEffect<P> {
    /// Send `dssu` to `peer`.
    Status {
        /// The recipient.
        peer: P,
        /// The update.
        update: DsStatusUpdate,
    },
    /// Send `dsf` (the session and the unsigned final transaction) to `peer`.
    FinalTx {
        /// The recipient.
        peer: P,
        /// The session.
        session_id: i32,
        /// The final transaction.
        tx: Transaction,
    },
    /// Send `dsc` to `peer`.
    Complete {
        /// The recipient.
        peer: P,
        /// The session.
        session_id: i32,
        /// The outcome.
        message: PoolMessage,
    },
    /// Relay a queue this masternode signed and added to its [`QueueManager`] (`RelayDSQ`).
    RelayQueue(DsQueue),
    /// `ConsumeCollateral`: `ATMPIfSaneFee(collateral)` and, when the mempool takes it, relay
    /// it.
    ConsumeCollateral(Transaction),
    /// Keep this signed DSTX (`AddDSTX`).
    AddDstx {
        /// The transaction's hash.
        txid: TxHash,
        /// The DSTX.
        dstx: DsTx,
    },
    /// Relay `MSG_DSTX` for the committed transaction (`PeerRelayInv`).
    RelayDstxInv(TxHash),
}

/// An input of an entry (Core's `CTxDSIn`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EntryInput {
    /// The input; its script is the participant's signature once signed.
    pub txin: TxIn,
    /// `prevPubKey`, memory only: empty for an entry read from the wire.
    pub prev_pub_key: Vec<u8>,
    /// `fHasSig`.
    pub has_sig: bool,
}

/// A participant's entry (Core's `CCoinJoinEntry` on the masternode).
#[derive(Clone, Debug)]
pub struct ServerEntry<P> {
    /// `vecTxDSIn`.
    pub inputs: Vec<EntryInput>,
    /// `vecTxOut`.
    pub outputs: Vec<TxOut>,
    /// `txCollateral`.
    pub collateral: Transaction,
    /// The collateral's hash.
    pub collateral_txid: TxHash,
    /// `addr`: the participant.
    pub peer: P,
}

impl<P> ServerEntry<P> {
    /// `GetMixShape()`.
    pub fn mix_shape(&self) -> MixShape {
        MixShape::of_entry(self.inputs.len(), self.outputs.len())
    }

    // Core: coinjoin/coinjoin.cpp:27 CCoinJoinEntry::AddScriptSig
    /// `AddScriptSig`: sign the unsigned input with `txin`'s outpoint and sequence.
    fn add_script_sig(&mut self, txin: &TxIn) -> bool {
        for input in &mut self.inputs {
            if input.txin.prevout() == txin.prevout() && input.txin.sequence() == txin.sequence() {
                if input.has_sig {
                    return false;
                }
                input.txin = dash_primitives::TxIn::new(
                    input.txin.prevout(),
                    txin.script_sig().clone(),
                    input.txin.sequence(),
                );
                input.has_sig = true;
                return true;
            }
        }
        false
    }
}

/// `dss` inputs as the node read them.
#[derive(Clone, Debug)]
pub enum SignedInputs {
    /// The inputs.
    Inputs(Vec<TxIn>),
    /// More than [`PoolLimits::max_pool_in_outs`] (BS9b's `DsSignedInputs::check_payload`).
    OverCap,
}

/// A transaction as Core's `CMutableTransaction()` makes it: version 2, normal, no lock time.
fn empty_transaction() -> Transaction {
    Transaction::new(2, TxType::Spend, Vec::new(), Vec::new(), 0, Vec::new())
}

/// The masternode's mixing session (Core's `CCoinJoinServer` state).
#[derive(Clone, Debug)]
pub struct ServerSession<P> {
    limits: PoolLimits,
    state: PoolState,
    session_id: i32,
    session_denom: i32,
    entries: Vec<ServerEntry<P>>,
    final_tx: Transaction,
    time_last_successful_step: i64,
    collaterals: Vec<(TxHash, Transaction)>,
    collateral_prevouts: BTreeSet<OutPoint>,
    rebalance_session: bool,
    has_legacy_participant: bool,
    declared_shapes: BTreeMap<TxHash, MixShape>,
    queues: QueueManager,
}

impl<P: Clone + Eq> ServerSession<P> {
    /// An idle session for a chain with these pool limits.
    pub fn new(limits: PoolLimits) -> Self {
        Self {
            limits,
            state: PoolState::Idle,
            session_id: 0,
            session_denom: 0,
            entries: Vec::new(),
            final_tx: empty_transaction(),
            time_last_successful_step: 0,
            collaterals: Vec::new(),
            collateral_prevouts: BTreeSet::new(),
            rebalance_session: false,
            has_legacy_participant: false,
            declared_shapes: BTreeMap::new(),
            queues: QueueManager::default(),
        }
    }

    /// `nState`.
    pub fn state(&self) -> PoolState {
        self.state
    }

    /// `nSessionID`, 0 without a session.
    pub fn session_id(&self) -> i32 {
        self.session_id
    }

    /// `nSessionDenom`.
    pub fn session_denom(&self) -> i32 {
        self.session_denom
    }

    /// `vecEntries`.
    pub fn entries(&self) -> &[ServerEntry<P>] {
        &self.entries
    }

    /// The final transaction, as signed so far.
    pub fn final_tx(&self) -> &Transaction {
        &self.final_tx
    }

    /// `m_session_collaterals.size()`.
    pub fn collateral_count(&self) -> usize {
        self.collaterals.len()
    }

    /// The masternode's queue manager (`m_queueman`): its own queues and the ones it received.
    /// `AlreadyHave(MSG_DSQ)` and `getdata` read it.
    pub fn queues(&self) -> &QueueManager {
        &self.queues
    }

    // Core: coinjoin/server.cpp:128 CCoinJoinServer::ProcessDSQUEUE
    /// `ProcessDSQUEUE`: a queue received from a peer at the adjusted time `now`. It is checked
    /// against, and kept in, the same manager as the masternode's own queues, so a session reset
    /// clears it and a kept queue of this masternode's answers a `dsa` with
    /// [`PoolMessage::ErrRecent`], as in Core.
    pub fn process_dsq<T: TipMasternodes + ?Sized, M: MixingMeta + ?Sized>(
        &mut self,
        counters: &mut M,
        mns: &T,
        dsq: DsQueue,
        now: i64,
    ) -> DsqOutcome {
        server_process_dsq(&mut self.queues, counters, mns, dsq, now)
    }

    // Core: coinjoin/server.cpp:297 CCoinJoinServer::SetNull
    fn set_null(&mut self, now: i64) {
        self.collaterals.clear();
        self.collateral_prevouts.clear();
        self.rebalance_session = false;
        self.has_legacy_participant = false;
        self.declared_shapes.clear();
        // Core: coinjoin/coinjoin.cpp:157 CCoinJoinBaseSession::SetNull
        self.state = PoolState::Idle;
        self.session_id = 0;
        self.session_denom = 0;
        self.entries.clear();
        self.final_tx = with_in_outs(&self.final_tx, Vec::new(), Vec::new());
        self.time_last_successful_step = now;
        self.queues.clear();
    }

    // Core: coinjoin/server.cpp:1407 CCoinJoinServer::SetState
    fn set_state(&mut self, state: PoolState, now: i64) {
        // Core refuses ERROR here; a masternode never enters it, and no caller asks for it.
        debug_assert_ne!(state, PoolState::Error);
        self.time_last_successful_step = now;
        self.state = state;
    }

    fn status(&self, update: PoolStatusUpdate, message: PoolMessage) -> DsStatusUpdate {
        DsStatusUpdate::new(self.session_id, self.state as i32, update as i32, message as i32)
    }

    // Core: coinjoin/server.cpp:1325 CCoinJoinServer::PushStatus
    fn push_status(
        &self,
        fx: &mut Vec<ServerEffect<P>>,
        peer: &P,
        update: PoolStatusUpdate,
        message: PoolMessage,
    ) {
        fx.push(ServerEffect::Status {
            peer: peer.clone(),
            update: self.status(update, message),
        });
    }

    /// `PushStatus(peer, STATUS_REJECTED, message)`.
    fn reject(&self, fx: &mut Vec<ServerEffect<P>>, peer: &P, message: PoolMessage) {
        self.push_status(fx, peer, PoolStatusUpdate::Rejected, message);
    }

    // Core: coinjoin/server.cpp:687 CCoinJoinServer::HasTimedOut
    fn has_timed_out(&self, now: i64) -> bool {
        if self.state == PoolState::Idle {
            return false;
        }
        now.saturating_sub(self.time_last_successful_step) >= state_timeout(self.state)
    }

    // Core: coinjoin/server.cpp:1066 CCoinJoinServer::GetDeclaredSideCounts
    fn declared_side_counts(&self) -> MixSideCounts {
        self.declared_shapes.values().copied().collect()
    }

    /// `GetMixSideCountsLocked()`: the live entries' sides.
    fn entry_side_counts(&self) -> MixSideCounts {
        self.entries.iter().map(ServerEntry::mix_shape).collect()
    }

    fn collateral_count_i32(&self) -> i32 {
        i32::try_from(self.collaterals.len()).unwrap_or(i32::MAX)
    }

    // Core: coinjoin/server.cpp:1282 CCoinJoinServer::IsSessionReady
    /// `IsSessionReady`: accepting entries, or queued with both sides covered and either the
    /// most participants or, timed out, the fewest.
    pub fn is_session_ready(&self, now: i64) -> bool {
        match self.state {
            PoolState::Queue => {
                // Privacy: never mix with a lone participant on a side.
                if !self.declared_side_counts().is_covered() {
                    return false;
                }
                let n = self.collateral_count_i32();
                n >= self.limits.max_participants
                    || (self.has_timed_out(now) && n >= self.limits.min_participants)
            }
            PoolState::AcceptingEntries => true,
            _ => false,
        }
    }

    /// Whether `peer` is one of the entries in this session.
    // Core: coinjoin/server.cpp:243-250 CCoinJoinServer::ProcessDSSIGNFINALTX
    pub fn is_session_participant(&self, peer: &P) -> bool {
        self.entries.iter().any(|entry| entry.peer == *peer)
    }

    /// The rejection code to send before decoding a `dsa`, if the session is already full.
    // Core: coinjoin/server.cpp:70-75 CCoinJoinServer::ProcessDSACCEPT
    pub fn pre_decode_dsa_reply(&self, now: i64) -> Option<PoolMessage> {
        self.is_session_ready(now).then_some(PoolMessage::ErrQueueFull)
    }

    /// The rejection code to send before decoding a `dsi`, if the session is not ready for entries.
    // Core: coinjoin/server.cpp:202-207 CCoinJoinServer::ProcessDSVIN
    pub fn pre_decode_dsi_reply(&self, now: i64) -> Option<PoolMessage> {
        (!self.is_session_ready(now)).then_some(PoolMessage::ErrSession)
    }

    /// The wrong-state rejection code to send before reading a `dss` count or its items.
    // Core: coinjoin/server.cpp:230-241 CCoinJoinServer::ProcessDSSIGNFINALTX
    pub fn pre_decode_dss_state_reply(&self) -> Option<PoolMessage> {
        (self.state != PoolState::Signing).then_some(PoolMessage::ErrSession)
    }

    /// The non-participant rejection code to send before reading a `dss` count or its items.
    /// Check [`Self::pre_decode_dss_state_reply`] first.
    // Core: coinjoin/server.cpp:243-250 CCoinJoinServer::ProcessDSSIGNFINALTX
    pub fn pre_decode_dss_participant_reply(&self, peer: &P) -> Option<PoolMessage> {
        (!self.is_session_participant(peer)).then_some(PoolMessage::ErrInvalidInput)
    }

    /// The over-cap rejection code to send after reading a valid `dss` count, before its items.
    /// Check the state and participant gates first. Malformed CompactSize counts must fail
    /// decoding rather than reaching this gate.
    // Core: coinjoin/server.cpp:254-263 CCoinJoinServer::ProcessDSSIGNFINALTX
    pub fn pre_decode_dss_count_reply(&self, item_count: usize) -> Option<PoolMessage> {
        (item_count > self.limits.max_pool_in_outs()).then_some(PoolMessage::ErrMaximum)
    }

    fn has_session_collateral(&self, txid: &TxHash) -> bool {
        self.collaterals.iter().any(|(id, _)| id == txid)
    }

    /// Sign, keep and relay a queue for the session's denomination (`CreateNewSession`,
    /// `CheckForCompleteQueue`).
    fn announce_queue<E: ServerEnv<Peer = P>>(
        &mut self,
        env: &mut E,
        ready: bool,
        fx: &mut Vec<ServerEffect<P>>,
    ) {
        let (denom, pro_tx_hash, time) =
            (self.session_denom, env.self_pro_tx_hash(), env.adjusted_time());
        let unsigned = DsQueue::new(denom, pro_tx_hash, time, ready, Vec::new());
        let dsq = DsQueue::new(
            denom,
            pro_tx_hash,
            time,
            ready,
            env.sign_basic(&unsigned.signature_hash()),
        );
        fx.push(ServerEffect::RelayQueue(dsq.clone()));
        self.queues.add(QueueEntry::new(dsq, env.self_outpoint()));
    }

    // Core: coinjoin/server.cpp:66 CCoinJoinServer::ProcessDSACCEPT
    /// A `dsa` from `peer`, whose connection runs at `peer_version`.
    pub fn process_dsa<E: ServerEnv<Peer = P>, M: MixingMeta + ?Sized>(
        &mut self,
        env: &mut E,
        counters: &M,
        peer: &P,
        peer_version: ProtocolVersion,
        dsa: &DsAccept,
    ) -> Vec<ServerEffect<P>> {
        let mut fx = Vec::new();
        if self.is_session_ready(env.now()) {
            // Too many users in this session already.
            self.reject(&mut fx, peer, PoolMessage::ErrQueueFull);
            return fx;
        }
        let Some(pro_tx_hash) = env.valid_self() else {
            self.reject(&mut fx, peer, PoolMessage::ErrMnList);
            return fx;
        };
        if self.collaterals.is_empty()
            && (self.queues.has_any_queue_from_masternode(&env.self_outpoint())
                || counters.is_mixing_threshold_exceeded(&pro_tx_hash, env.enabled_count()))
        {
            // The last queue is still out, or was too recent.
            self.reject(&mut fx, peer, PoolMessage::ErrRecent);
            return fx;
        }

        let result = if self.session_id == 0 {
            self.create_new_session(env, dsa, peer_version, &mut fx)
        } else {
            self.add_user_to_existing_session(env, dsa, peer_version)
        };
        match result {
            Ok(message) => self.push_status(&mut fx, peer, PoolStatusUpdate::Accepted, message),
            Err(message) => self.reject(&mut fx, peer, message),
        }
        fx
    }

    // Core: coinjoin/server.cpp:1038 CCoinJoinServer::IsAcceptableDSA
    fn is_acceptable_dsa<E: ServerEnv<Peer = P>>(
        env: &E,
        dsa: &DsAccept,
    ) -> Result<(), PoolMessage> {
        if !denom::is_valid_denomination(dsa.denom()) {
            return Err(PoolMessage::ErrDenom);
        }
        if !is_collateral_valid(env, dsa.collateral()) {
            return Err(PoolMessage::ErrInvalidCollateral);
        }
        Ok(())
    }

    // Core: coinjoin/server.h:85 CCoinJoinServer::SessionCollaterals::Add
    fn commit_session_collateral(
        &mut self,
        txid: TxHash,
        collateral: &Transaction,
        shape: MixShape,
    ) {
        self.declared_shapes.entry(txid).or_insert(shape);
        self.collaterals.push((txid, collateral.clone()));
        self.collateral_prevouts.extend(collateral.inputs().iter().map(|i| i.prevout()));
    }

    // Core: coinjoin/server.cpp:1076 CCoinJoinServer::CreateNewSession
    fn create_new_session<E: ServerEnv<Peer = P>>(
        &mut self,
        env: &mut E,
        dsa: &DsAccept,
        peer_version: ProtocolVersion,
        fx: &mut Vec<ServerEffect<P>>,
    ) -> Result<PoolMessage, PoolMessage> {
        // A new session only starts idle.
        if self.state != PoolState::Idle {
            return Err(PoolMessage::ErrMode);
        }
        Self::is_acceptable_dsa(env, dsa)?;
        if !dsa.has_valid_flags() {
            return Err(PoolMessage::ErrVersion);
        }
        // A rebalance needs V24 and a peer that speaks it; the session takes its capability
        // from what this dsa asks for.
        if dsa.is_rebalance()
            && (peer_version < ProtocolVersion::COINJOIN_REBALANCE || !env.v24_active_at_tip())
        {
            return Err(PoolMessage::ErrVersion);
        }

        let now = env.now();
        // Core: coinjoin/server.cpp:1128 GetRand<int>(999999) + 1. Reduced again here, so an
        // environment that breaks `rand_below`'s contract still gives an id in 1..=999_999.
        let draw = env.rand_below(999_999) % 999_999;
        self.session_id = i32::try_from(draw).unwrap_or(0) + 1;
        self.session_denom = dsa.denom();
        self.rebalance_session = dsa.is_rebalance();
        self.has_legacy_participant = peer_version < ProtocolVersion::COINJOIN_REBALANCE;
        self.set_state(PoolState::Queue, now);
        let txid = crate::dstx::txid(dsa.collateral());
        self.commit_session_collateral(txid, dsa.collateral(), dsa.declared_shape());

        self.announce_queue(env, false, fx);
        Ok(PoolMessage::MsgNoErr)
    }

    // Core: coinjoin/server.cpp:1159 CCoinJoinServer::AddUserToExistingSession
    fn add_user_to_existing_session<E: ServerEnv<Peer = P>>(
        &mut self,
        env: &mut E,
        dsa: &DsAccept,
        peer_version: ProtocolVersion,
    ) -> Result<PoolMessage, PoolMessage> {
        if self.session_id == 0 || self.state != PoolState::Queue {
            return Err(PoolMessage::ErrMode);
        }
        if self.is_session_ready(env.now()) {
            return Err(PoolMessage::ErrQueueFull);
        }
        Self::is_acceptable_dsa(env, dsa)?;
        if dsa.denom() != self.session_denom {
            return Err(PoolMessage::ErrDenom);
        }
        if !dsa.has_valid_flags() {
            return Err(PoolMessage::ErrVersion);
        }
        let legacy = peer_version < ProtocolVersion::COINJOIN_REBALANCE;
        // A rebalance participant and a client that cannot check an unbalanced final
        // transaction never share a session; the first one to force the question decides.
        if dsa.is_rebalance() {
            if legacy || !env.v24_active_at_tip() || self.has_legacy_participant {
                return Err(PoolMessage::ErrVersion);
            }
        } else if legacy && self.rebalance_session {
            return Err(PoolMessage::ErrVersion);
        }
        if self.collateral_count_i32() >= self.limits.max_participants {
            return Err(PoolMessage::ErrQueueFull);
        }
        // Collaterals are only test-accepted: match on prevouts so a re-signed one is not a new
        // participant.
        if dsa.collateral().inputs().iter().any(|i| self.collateral_prevouts.contains(&i.prevout()))
        {
            return Err(PoolMessage::ErrAlreadyHave);
        }

        self.rebalance_session |= dsa.is_rebalance();
        self.has_legacy_participant |= legacy;
        let txid = crate::dstx::txid(dsa.collateral());
        self.commit_session_collateral(txid, dsa.collateral(), dsa.declared_shape());
        Ok(PoolMessage::MsgNoErr)
    }

    // Core: coinjoin/server.cpp:200 CCoinJoinServer::ProcessDSVIN
    /// A `dsi` entry from `peer`.
    pub fn process_dsi<E: ServerEnv<Peer = P>>(
        &mut self,
        env: &mut E,
        peer: &P,
        entry: DsEntry,
    ) -> Vec<ServerEffect<P>> {
        let mut fx = Vec::new();
        if !self.is_session_ready(env.now()) {
            self.reject(&mut fx, peer, PoolMessage::ErrSession);
            return fx;
        }
        match self.add_entry(env, peer, entry, &mut fx) {
            Ok(message) => {
                self.push_status(&mut fx, peer, PoolStatusUpdate::Accepted, message);
                self.check_pool(env, &mut fx);
                self.relay_status(env, PoolStatusUpdate::Accepted, PoolMessage::MsgNoErr, &mut fx);
            }
            Err(message) => self.reject(&mut fx, peer, message),
        }
        fx
    }

    // Core: coinjoin/server.cpp:829 CCoinJoinServer::AddEntry
    fn add_entry<E: ServerEnv<Peer = P>>(
        &mut self,
        env: &mut E,
        peer: &P,
        entry: DsEntry,
        fx: &mut Vec<ServerEffect<P>>,
    ) -> Result<PoolMessage, PoolMessage> {
        if self.session_id == 0 || self.state != PoolState::AcceptingEntries {
            return Err(PoolMessage::ErrSession);
        }
        if self.entries.len() >= self.collaterals.len() {
            return Err(PoolMessage::ErrEntriesFull);
        }
        // Entries are keyed one-to-one to the collaterals accepted with a dsa. Don't charge:
        // it may be a replay of someone else's collateral.
        let collateral_txid = crate::dstx::txid(entry.collateral());
        if !self.has_session_collateral(&collateral_txid) {
            return Err(PoolMessage::ErrSession);
        }
        if self.entries.iter().any(|e| e.collateral_txid == collateral_txid) {
            return Err(PoolMessage::ErrAlreadyHave);
        }
        let declared =
            self.declared_shapes.get(&collateral_txid).copied().unwrap_or(MixShape::Standard);

        // The entry rules are the session's, fixed when it was created.
        let max_entry_size = if self.rebalance_session {
            denom::PROMOTION_RATIO_COUNT
        } else {
            ENTRY_MAX_SIZE
        };
        if entry.inputs().len() > max_entry_size || entry.outputs().len() > max_entry_size {
            fx.push(ServerEffect::ConsumeCollateral(entry.collateral().clone()));
            return Err(PoolMessage::ErrMaximum);
        }
        if !is_collateral_valid(env, entry.collateral()) {
            return Err(PoolMessage::ErrInvalidCollateral);
        }
        let shape = MixShape::of_entry(entry.inputs().len(), entry.outputs().len());
        if shape == MixShape::Unknown {
            fx.push(ServerEffect::ConsumeCollateral(entry.collateral().clone()));
            return Err(PoolMessage::ErrSizeMismatch);
        }
        // Admission counted on the declared shapes, so deviating costs the collateral.
        if self.rebalance_session && shape != declared {
            fx.push(ServerEffect::ConsumeCollateral(entry.collateral().clone()));
            return Err(PoolMessage::ErrSizeMismatch);
        }
        // The same input twice: can't tell who is malicious, so don't charge.
        let pooled = |prevout: &OutPoint| {
            self.entries.iter().any(|e| e.inputs.iter().any(|i| i.txin.prevout() == *prevout))
        };
        if entry.inputs().iter().any(|txin| pooled(&txin.prevout())) {
            return Err(PoolMessage::ErrAlreadyHave);
        }
        if let Err(rejection) = is_valid_in_outs(
            env,
            entry.inputs(),
            entry.outputs(),
            self.session_denom,
            self.rebalance_session,
            InOutsKind::Entry,
        ) {
            if rejection.consume_collateral {
                fx.push(ServerEffect::ConsumeCollateral(entry.collateral().clone()));
            }
            return Err(rejection.message);
        }

        self.entries.push(ServerEntry {
            inputs: entry
                .inputs()
                .iter()
                .cloned()
                .map(|txin| EntryInput {
                    txin,
                    prev_pub_key: Vec::new(),
                    has_sig: false,
                })
                .collect(),
            outputs: entry.outputs().clone(),
            collateral: entry.collateral().clone(),
            collateral_txid,
            peer: peer.clone(),
        });
        Ok(PoolMessage::MsgEntriesAdded)
    }

    // Core: coinjoin/server.cpp:230 CCoinJoinServer::ProcessDSSIGNFINALTX
    /// A `dss` from `peer`.
    pub fn process_dss<E: ServerEnv<Peer = P>>(
        &mut self,
        env: &mut E,
        peer: &P,
        inputs: SignedInputs,
    ) -> Vec<ServerEffect<P>> {
        let mut fx = Vec::new();
        // Only participants, and only while signing: otherwise anyone could abort the session.
        if self.state != PoolState::Signing {
            self.reject(&mut fx, peer, PoolMessage::ErrSession);
            return fx;
        }
        if !self.entries.iter().any(|e| e.peer == *peer) {
            self.reject(&mut fx, peer, PoolMessage::ErrInvalidInput);
            return fx;
        }
        let SignedInputs::Inputs(inputs) = inputs else {
            self.reject(&mut fx, peer, PoolMessage::ErrMaximum);
            return fx;
        };
        for txin in &inputs {
            if !self.add_script_sig(txin) {
                self.relay_status(env, PoolStatusUpdate::Rejected, PoolMessage::MsgNoErr, &mut fx);
                return fx;
            }
        }
        self.check_pool(env, &mut fx);
        fx
    }

    // Core: coinjoin/server.cpp:782 CCoinJoinServer::IsInputScriptSigValid
    /// `IsInputScriptSigValid`: `txin`'s script against the pooled input's `prevPubKey`, in a
    /// transaction of the entries in their order (not the final transaction's). For an entry read
    /// from the wire `prevPubKey` is empty; Core checks it all the same (CJ0 notes D15; possible
    /// security note: see manager).
    fn is_input_script_sig_valid(&self, txin: &TxIn) -> bool {
        let (mut inputs, mut outputs) = (Vec::new(), Vec::new());
        let mut index = None;
        let mut prev_pub_key: &[u8] = &[];
        for entry in &self.entries {
            outputs.extend(entry.outputs.iter().cloned());
            for input in &entry.inputs {
                if input.txin.prevout() == txin.prevout() {
                    index = Some(inputs.len());
                    prev_pub_key = &input.prev_pub_key;
                }
                inputs.push(input.txin.clone());
            }
        }
        let Some(index) = index else {
            return false;
        };
        let signed: &TxIn = &inputs[index];
        inputs[index] = TxIn::new(signed.prevout(), txin.script_sig().clone(), signed.sequence());
        let tx = with_in_outs(&empty_transaction(), inputs, outputs);
        let data = TxSigData::new(&tx);
        let checker = TransactionSignatureChecker::new(&data, index, &DirectVerifier);
        verify_script(
            txin.script_sig().as_bytes(),
            prev_pub_key,
            ScriptFlags::P2SH | ScriptFlags::STRICTENC,
            &checker,
        )
        .is_ok()
    }

    // Core: coinjoin/server.cpp:991 CCoinJoinServer::AddScriptSig
    fn add_script_sig(&mut self, txin: &TxIn) -> bool {
        let seen = self
            .entries
            .iter()
            .any(|e| e.inputs.iter().any(|i| i.txin.script_sig() == txin.script_sig()));
        if seen || !self.is_input_script_sig_valid(txin) {
            return false;
        }
        let inputs = self
            .final_tx
            .inputs()
            .iter()
            .map(|input| {
                if input.prevout() == txin.prevout() && input.sequence() == txin.sequence() {
                    TxIn::new(input.prevout(), txin.script_sig().clone(), input.sequence())
                } else {
                    input.clone()
                }
            })
            .collect();
        self.final_tx = with_in_outs(&self.final_tx, inputs, self.final_tx.outputs().clone());
        self.entries.iter_mut().any(|e| e.add_script_sig(txin))
    }

    // Core: coinjoin/server.cpp:1029 CCoinJoinServer::IsSignaturesComplete
    fn is_signatures_complete(&self) -> bool {
        self.entries.iter().all(|e| e.inputs.iter().all(|i| i.has_sig))
    }

    // Core: coinjoin/server.cpp:313 CCoinJoinServer::CheckPool
    fn check_pool<E: ServerEnv<Peer = P>>(&mut self, env: &mut E, fx: &mut Vec<ServerEffect<P>>) {
        let sides = self.entry_side_counts();
        if self.state == PoolState::AcceptingEntries && self.entries.len() == self.collaterals.len()
        {
            if sides.is_covered() {
                self.create_final_transaction(env, false, fx);
                return;
            }
            let (session_id, participants) = (self.session_id, self.participant_addrs());
            self.set_null(env.now());
            Self::relay_completed_transaction(
                env,
                session_id,
                &participants,
                PoolMessage::ErrSession,
                fx,
            );
            return;
        }
        // Timed out accepting entries: finish with the participants that sent one.
        if self.state == PoolState::AcceptingEntries
            && self.has_timed_out(env.now())
            && sides.is_covered()
            && self.entries.len() >= self.limits.min()
        {
            self.create_final_transaction(env, true, fx);
            return;
        }
        if self.state == PoolState::Signing && self.is_signatures_complete() {
            self.commit_final_transaction(env, fx);
        }
    }

    // Core: coinjoin/server.cpp:390 CCoinJoinServer::CreateFinalTransaction
    fn create_final_transaction<E: ServerEnv<Peer = P>>(
        &mut self,
        env: &mut E,
        charge_fees: bool,
        fx: &mut Vec<ServerEffect<P>>,
    ) {
        if self.state != PoolState::AcceptingEntries
            || self.session_id == 0
            || self.entries.len() < self.limits.min()
            || !self.entry_side_counts().is_covered()
        {
            return;
        }
        let collateral_to_charge = if charge_fees {
            self.select_collateral_to_charge(env)
        } else {
            None
        };
        let (mut inputs, mut outputs) = (Vec::new(), Vec::new());
        for entry in &self.entries {
            outputs.extend(entry.outputs.iter().cloned());
            inputs.extend(entry.inputs.iter().map(|i| i.txin.clone()));
        }
        let mut tx = with_in_outs(&empty_transaction(), inputs, outputs);
        sort_bip69(&mut tx);
        self.final_tx = tx;
        self.set_state(PoolState::Signing, env.now());
        self.relay_final_transaction(env, fx);
        if let Some(collateral) = collateral_to_charge {
            fx.push(ServerEffect::ConsumeCollateral(collateral));
        }
    }

    // Core: coinjoin/server.cpp:453 CCoinJoinServer::CommitFinalTransaction
    fn commit_final_transaction<E: ServerEnv<Peer = P>>(
        &mut self,
        env: &mut E,
        fx: &mut Vec<ServerEffect<P>>,
    ) {
        let tx = self.final_tx.clone();
        let txid = crate::dstx::txid(&tx);
        let (session_id, participants) = (self.session_id, self.participant_addrs());
        if !env.commit_to_mempool(&tx, &txid) {
            Self::relay_completed_transaction(
                env,
                session_id,
                &participants,
                PoolMessage::ErrInvalidTx,
                fx,
            );
            self.set_null(env.now());
            return;
        }
        if !env.has_dstx(&txid) {
            let (pro_tx_hash, sig_time) = (env.self_pro_tx_hash(), env.adjusted_time());
            let unsigned = DsTx::new(tx, pro_tx_hash, Vec::new(), sig_time);
            let sig = env.sign_basic(&crate::dstx::dstx_signature_hash(&unsigned));
            let dstx = DsTx::new(unsigned.tx().clone(), pro_tx_hash, sig, sig_time);
            fx.push(ServerEffect::AddDstx {
                txid,
                dstx,
            });
        }
        fx.push(ServerEffect::RelayDstxInv(txid));
        Self::relay_completed_transaction(
            env,
            session_id,
            &participants,
            PoolMessage::MsgSuccess,
            fx,
        );
        self.charge_random_fees(env, fx);
        self.set_null(env.now());
    }

    // Core: coinjoin/server.cpp:536 CCoinJoinServer::SelectCollateralToCharge
    /// Select a random missing entry or signature to charge, unless all or nearly all are missing.
    fn select_collateral_to_charge<E: ServerEnv<Peer = P>>(
        &self,
        env: &mut E,
    ) -> Option<Transaction> {
        if env.rand_below(100) > 33 {
            return None;
        }
        let mut offenders: Vec<&Transaction> = Vec::new();
        if self.state == PoolState::AcceptingEntries {
            for (txid, collateral) in &self.collaterals {
                if !self.entries.iter().any(|e| e.collateral_txid == *txid) {
                    offenders.push(collateral);
                }
            }
        }
        if self.state == PoolState::Signing {
            // One offence per unsigned input.
            for entry in &self.entries {
                for input in &entry.inputs {
                    if !input.has_sig {
                        offenders.push(&entry.collateral);
                    }
                }
            }
        }
        if offenders.is_empty() {
            return None;
        }
        let total = self.collaterals.len();
        if offenders.len() >= total.wrapping_sub(1) && env.rand_below(100) > 33 {
            return None;
        }
        if offenders.len() >= total {
            return None;
        }
        // Core shuffles and charges the first: one uniformly chosen offence.
        let pick =
            usize::try_from(env.rand_below(u32::try_from(offenders.len()).unwrap_or(u32::MAX)))
                .unwrap_or(0);
        Some(offenders[pick % offenders.len()].clone())
    }

    // Core: coinjoin/server.cpp:607 CCoinJoinServer::ChargeRandomFees
    /// `ChargeRandomFees`: each collateral in turn with probability 11/100, stopping at the first
    /// that is spared.
    fn charge_random_fees<E: ServerEnv<Peer = P>>(
        &self,
        env: &mut E,
        fx: &mut Vec<ServerEffect<P>>,
    ) {
        for (_, collateral) in &self.collaterals {
            if env.rand_below(100) > 10 {
                return;
            }
            fx.push(ServerEffect::ConsumeCollateral(collateral.clone()));
        }
    }

    // Core: coinjoin/server.cpp:1304 CCoinJoinServer::RelayFinalTransaction
    fn relay_final_transaction<E: ServerEnv<Peer = P>>(
        &mut self,
        env: &mut E,
        fx: &mut Vec<ServerEffect<P>>,
    ) {
        for i in 0..self.entries.len() {
            let peer = self.entries[i].peer.clone();
            if !env.is_connected(&peer) {
                self.relay_status(env, PoolStatusUpdate::Rejected, PoolMessage::MsgNoErr, fx);
                return;
            }
            fx.push(ServerEffect::FinalTx {
                peer,
                session_id: self.session_id,
                tx: self.final_tx.clone(),
            });
        }
    }

    // Core: coinjoin/server.cpp:1331 CCoinJoinServer::RelayStatus
    fn relay_status<E: ServerEnv<Peer = P>>(
        &mut self,
        env: &mut E,
        update: PoolStatusUpdate,
        message: PoolMessage,
        fx: &mut Vec<ServerEffect<P>>,
    ) {
        let mut disconnected = 0;
        for entry in &self.entries {
            if env.is_connected(&entry.peer) {
                self.push_status(fx, &entry.peer, update, message);
            } else {
                disconnected += 1;
            }
        }
        if disconnected == 0 {
            return;
        }
        // Someone left: tell the rest the session is over.
        for entry in &self.entries {
            if env.is_connected(&entry.peer) {
                self.reject(fx, &entry.peer, PoolMessage::MsgNoErr);
            }
        }
        if disconnected == self.entries.len() {
            // Probably our own connection: reset without charging.
            self.set_null(env.now());
        }
    }

    // Core: coinjoin/server.cpp:1368 CCoinJoinServer::RelayCompletedTransaction
    fn relay_completed_transaction<E: ServerEnv<Peer = P>>(
        env: &E,
        session_id: i32,
        participants: &[P],
        message: PoolMessage,
        fx: &mut Vec<ServerEffect<P>>,
    ) {
        for peer in participants {
            if env.is_connected(peer) {
                fx.push(ServerEffect::Complete {
                    peer: peer.clone(),
                    session_id,
                    message,
                });
            }
        }
    }

    // Core: coinjoin/server.cpp:1386 CCoinJoinServer::GetParticipantAddrs
    fn participant_addrs(&self) -> Vec<P> {
        self.entries.iter().map(|entry| entry.peer.clone()).collect()
    }

    // Core: coinjoin/server.cpp:758 CCoinJoinServer::CheckForCompleteQueue
    fn check_for_complete_queue<E: ServerEnv<Peer = P>>(
        &mut self,
        env: &mut E,
        fx: &mut Vec<ServerEffect<P>>,
    ) {
        if self.state != PoolState::Queue || !self.is_session_ready(env.now()) {
            return;
        }
        self.set_state(PoolState::AcceptingEntries, env.now());
        self.announce_queue(env, true, fx);
    }

    // Core: coinjoin/server.cpp:696 ShouldResetTimedOutSession
    fn should_reset_timed_out_session(&self, now: i64) -> bool {
        if !self.has_timed_out(now) {
            return false;
        }
        let can_advance = match self.state {
            PoolState::Queue => self.is_session_ready(now),
            PoolState::AcceptingEntries => {
                self.entries.len() == self.collaterals.len()
                    || (self.entries.len() >= self.limits.min()
                        && self.entry_side_counts().is_covered())
            }
            PoolState::Signing => self.is_signatures_complete(),
            _ => false,
        };
        !can_advance
    }

    // Core: coinjoin/server.cpp:712 CCoinJoinServer::CheckTimeout
    fn check_timeout<E: ServerEnv<Peer = P>>(
        &mut self,
        env: &mut E,
        fx: &mut Vec<ServerEffect<P>>,
    ) {
        self.queues.check_queue(env.adjusted_time());
        if !self.should_reset_timed_out_session(env.now()) {
            return;
        }
        let collateral_to_charge = self.select_collateral_to_charge(env);
        self.set_null(env.now());
        if let Some(collateral) = collateral_to_charge {
            fx.push(ServerEffect::ConsumeCollateral(collateral));
        }
    }

    // Core: coinjoin/server.cpp:1421 CCoinJoinServer::Schedule
    /// The once-a-second step (run only once the chain is synced): complete the queue, check the
    /// pool, check the timeouts.
    pub fn tick<E: ServerEnv<Peer = P>>(&mut self, env: &mut E) -> Vec<ServerEffect<P>> {
        let mut fx = Vec::new();
        self.check_for_complete_queue(env, &mut fx);
        self.check_pool(env, &mut fx);
        self.check_timeout(env, &mut fx);
        fx
    }
}
