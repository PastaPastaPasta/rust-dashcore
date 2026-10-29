//! Mixing queues (`dsq`): their time bounds and signatures, the queue manager both sides keep,
//! the masternode mixing counters, and what a masternode and a client do with a received queue
//! (`src/coinjoin/coinjoin.{h,cpp}`, `server.cpp`, `walletman.cpp`, `masternode/meta.cpp`).

use std::collections::BTreeMap;

use dash_num::Hash256;
use dash_p2p_core::DsQueue;
use dash_pkc::bls::BlsSchemeKind;
use dash_pkc::bls::core::{BlsPkValue, BlsSigValue, BlsVerdict, verify_insecure};
use dash_primitives::{OutPoint, TxHash};
use dash_types::Hashable;

use crate::denom;
use crate::pool::QUEUE_TIMEOUT;

// Core: coinjoin/coinjoin.cpp:59 CCoinJoinQueue::IsTimeOutOfBounds
/// `IsTimeOutOfBounds(current_time)`: a negative time on either side, or more than
/// [`QUEUE_TIMEOUT`] seconds between them. `now` is the node's adjusted time in seconds.
pub const fn is_time_out_of_bounds(queue_time: i64, now: i64) -> bool {
    if now < 0 || queue_time < 0 {
        return true;
    }
    // Both are non-negative, so the difference cannot overflow.
    (now - queue_time).abs() > QUEUE_TIMEOUT
}

/// Core's `CBLSSignature(Span{vchSig}, false).VerifyInsecure(pk, hash, false)`, the check of
/// queue and DSTX signatures: basic-scheme decoding and verification. Callers treat
/// [`BlsVerdict::Unsupported`] as neither valid nor the sender's fault; the base SDK only answers
/// it for the legacy scheme, so it does not occur here (CJ0 notes D9).
pub(crate) fn verify_masternode_sig(
    sig: &[u8],
    operator_key: &BlsPkValue,
    hash: &Hash256,
) -> BlsVerdict {
    let sig = BlsSigValue::from_set_bytes_basic(sig);
    verify_insecure(&sig, operator_key, hash.as_bytes(), BlsSchemeKind::Basic)
}

// Core: coinjoin/coinjoin.cpp:49 CCoinJoinQueue::CheckSignature
/// `CCoinJoinQueue::CheckSignature`: the operator's signature over BS9b's
/// [`DsQueue::signature_hash`].
pub fn check_queue_signature(dsq: &DsQueue, operator_key: &BlsPkValue) -> BlsVerdict {
    verify_masternode_sig(&dsq.sig, operator_key, &dsq.signature_hash())
}

/// A queue as Core's `CCoinJoinQueue` holds it: the message, the masternode's collateral (memory
/// only: never on the wire, filled in from the masternode list) and whether this node tried it.
#[derive(Clone, Debug)]
pub struct QueueEntry {
    /// The queue as received.
    pub dsq: DsQueue,
    /// The masternode's collateral outpoint.
    pub masternode_outpoint: OutPoint,
    /// Whether a client already tried to join it (`fTried`).
    pub tried: bool,
    hash: Hash256,
}

impl QueueEntry {
    /// The queue `dsq` from the masternode with collateral `masternode_outpoint`, untried.
    pub fn new(dsq: DsQueue, masternode_outpoint: OutPoint) -> Self {
        let hash = dsq.hash();
        Self {
            dsq,
            masternode_outpoint,
            tried: false,
            hash,
        }
    }

    /// `GetHash()`, the inventory hash (BS9b's hash of the queue as written).
    pub fn hash(&self) -> Hash256 {
        self.hash
    }

    /// `fReady`.
    pub fn is_ready(&self) -> bool {
        bool::from(self.dsq.ready)
    }

    /// `IsTimeOutOfBounds` at the adjusted time `now`.
    pub fn is_time_out_of_bounds(&self, now: i64) -> bool {
        is_time_out_of_bounds(self.dsq.time, now)
    }

    // Core: coinjoin/coinjoin.h:290 CCoinJoinQueue::operator==
    /// Core's `operator==`: denomination, masternode, time and readiness.
    pub fn same_queue(&self, other: &QueueEntry) -> bool {
        self.dsq.denom == other.dsq.denom
            && self.masternode_outpoint == other.masternode_outpoint
            && self.dsq.time == other.dsq.time
            && self.is_ready() == other.is_ready()
    }
}

// Core: coinjoin/coinjoin.h:430 CoinJoinQueueManager
/// Core's `CoinJoinQueueManager`: the mixing queues in progress on the network, in arrival order.
///
/// Core's `Try*` methods give up when another thread holds the lock; here the caller owns the
/// manager, so they always run (CJ0 notes D7).
#[derive(Clone, Debug, Default)]
pub struct QueueManager {
    queues: Vec<QueueEntry>,
}

impl QueueManager {
    /// The queues, in arrival order.
    pub fn queues(&self) -> &[QueueEntry] {
        &self.queues
    }

    /// `GetQueueSize()`.
    pub fn len(&self) -> usize {
        self.queues.len()
    }

    /// Whether no queue is held.
    pub fn is_empty(&self) -> bool {
        self.queues.is_empty()
    }

    /// `SetNull()`.
    pub fn clear(&mut self) {
        self.queues.clear();
    }

    // Core: coinjoin/coinjoin.cpp:176 CoinJoinQueueManager::CheckQueue
    /// `CheckQueue()`: drop the queues out of bounds at `now`.
    pub fn check_queue(&mut self, now: i64) {
        self.queues.retain(|q| !q.is_time_out_of_bounds(now));
    }

    // Core: coinjoin/coinjoin.cpp:219 CoinJoinQueueManager::GetQueueItemAndTry
    /// `GetQueueItemAndTry`: the first untried queue in bounds at `now` (and at `denom_filter`
    /// unless it is 0), marked tried. Queues of another denomination stay untried.
    pub fn get_queue_item_and_try(&mut self, now: i64, denom_filter: i32) -> Option<QueueEntry> {
        let q = self.queues.iter_mut().find(|q| {
            !q.tried
                && !q.is_time_out_of_bounds(now)
                && (denom_filter == 0 || q.dsq.denom == denom_filter)
        })?;
        q.tried = true;
        Some(q.clone())
    }

    /// `HasQueue(hash)`.
    pub fn has_queue(&self, hash: &Hash256) -> bool {
        self.queues.iter().any(|q| q.hash == *hash)
    }

    /// `GetQueueFromHash(hash)`: the queue a `getdata` for `MSG_DSQ` is answered with.
    pub fn queue_from_hash(&self, hash: &Hash256) -> Option<&QueueEntry> {
        self.queues.iter().find(|q| q.hash == *hash)
    }

    /// `HasQueueFromMasternode(outpoint, fReady)`.
    pub fn has_queue_from_masternode(&self, outpoint: &OutPoint, ready: bool) -> bool {
        self.queues.iter().any(|q| q.masternode_outpoint == *outpoint && q.is_ready() == ready)
    }

    // Core: coinjoin/coinjoin.cpp:193 CoinJoinQueueManager::TryHasQueueFromMasternode
    /// `TryHasQueueFromMasternode(outpoint)`: a queue from that masternode, ready or not.
    pub fn has_any_queue_from_masternode(&self, outpoint: &OutPoint) -> bool {
        self.queues.iter().any(|q| q.masternode_outpoint == *outpoint)
    }

    // Core: coinjoin/coinjoin.cpp:200 CoinJoinQueueManager::TryCheckDuplicate
    /// `TryCheckDuplicate`: the same queue, or one from the same masternode with the same
    /// readiness.
    pub fn is_duplicate(&self, entry: &QueueEntry) -> bool {
        self.queues.iter().any(|q| {
            q.same_queue(entry)
                || (q.is_ready() == entry.is_ready()
                    && q.masternode_outpoint == entry.masternode_outpoint)
        })
    }

    /// `AddQueue`: append (the caller checked for duplicates).
    pub fn add(&mut self, entry: QueueEntry) {
        self.queues.push(entry);
    }
}

/// One masternode's mixing record in Core's `CMasternodeMetaInfo`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MasternodeMixing {
    /// `m_last_dsq`: the queue count when its last queue was allowed, 0 for never.
    pub last_dsq: i64,
    /// `m_mixing_tx_count`: mixing transactions since that queue.
    pub mixing_tx_count: i32,
}

// Core: masternode/meta.cpp:14 MASTERNODE_MAX_MIXING_TXES
/// The most mixing transactions a masternode may sign per queue it was allowed.
pub const MAX_MIXING_TXES: i32 = 5;

/// The CoinJoin part of Core's `CMasternodeMetaMan`: the network's queue count and each
/// masternode's record. The node persists it (Core's `mncache.dat`).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MixingCounters {
    /// `nDsqCount`: queues allowed so far.
    pub dsq_count: i64,
    /// The records, by ProRegTx hash.
    pub masternodes: BTreeMap<TxHash, MasternodeMixing>,
}

impl MixingCounters {
    // Core: masternode/meta.cpp:93 CMasternodeMetaMan::IsMixingThresholdExceeded
    /// `IsMixingThresholdExceeded`: the masternode's last allowed queue is fewer than a fifth of
    /// the `mn_count` enabled masternodes' queues ago. An unknown masternode never exceeds it.
    pub fn is_mixing_threshold_exceeded(&self, pro_tx_hash: &TxHash, mn_count: i32) -> bool {
        let Some(mn) = self.masternodes.get(pro_tx_hash) else {
            return false;
        };
        let threshold = mn.last_dsq.saturating_add(i64::from(mn_count / 5));
        mn.last_dsq != 0 && threshold > self.dsq_count
    }

    // Core: masternode/meta.cpp:110 CMasternodeMetaMan::AllowMixing
    /// `AllowMixing`: count a queue and reset the masternode's transaction count.
    pub fn allow_mixing(&mut self, pro_tx_hash: &TxHash) {
        self.dsq_count += 1;
        let mn = self.masternodes.entry(*pro_tx_hash).or_default();
        mn.last_dsq = self.dsq_count;
        mn.mixing_tx_count = 0;
    }

    // Core: masternode/meta.cpp:118 CMasternodeMetaMan::DisallowMixing
    /// `DisallowMixing`: count one mixing transaction of the masternode.
    pub fn disallow_mixing(&mut self, pro_tx_hash: &TxHash) {
        let mn = self.masternodes.entry(*pro_tx_hash).or_default();
        mn.mixing_tx_count = mn.mixing_tx_count.saturating_add(1);
    }

    // Core: masternode/meta.cpp:124 CMasternodeMetaMan::IsValidForMixingTxes
    /// `IsValidForMixingTxes`: at most [`MAX_MIXING_TXES`] transactions since its last queue.
    pub fn is_valid_for_mixing_txes(&self, pro_tx_hash: &TxHash) -> bool {
        self.masternodes.get(pro_tx_hash).map_or(0, |mn| mn.mixing_tx_count) <= MAX_MIXING_TXES
    }
}

/// A masternode as the CoinJoin rules read it from the list.
#[derive(Clone, Debug)]
pub struct MixingMasternode {
    /// `proTxHash`.
    pub pro_tx_hash: TxHash,
    /// `collateralOutpoint`.
    pub collateral: OutPoint,
    /// `pdmnState->pubKeyOperator.Get()`.
    pub operator_key: BlsPkValue,
}

/// The masternode list at the active chain tip.
pub trait TipMasternodes {
    /// `GetListAtChainTip().GetValidMN(pro_tx_hash)`.
    fn valid_mn(&self, pro_tx_hash: &TxHash) -> Option<MixingMasternode>;

    /// `GetListAtChainTip().GetCounts().enabled()`.
    fn enabled_count(&self) -> i32;
}

/// What to do about a received queue. Whatever it is, the node first completes the object
/// request for its hash (Core's `PeerEraseObjectRequest` / `m_to_erase`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DsqOutcome {
    /// Penalise the sender by this score.
    Misbehaving(i32),
    /// Nothing more.
    Ignored,
    /// A masternode: the queue was added; forget its object request and relay it (Core's
    /// `PeerForgetObjectRequest` and `RelayDSQ`, see [`crate::relay::dsq_relay`]).
    Relay(Hash256),
    /// A client: the queue was added.
    Added,
    /// A client: a session mixing on the queue's masternode submitted its entry.
    Submitted,
}

/// The queue's masternode from the tip list: Core resolves a queue without an outpoint (every
/// queue read from the wire) by its ProRegTx hash, then looks it up again by that collateral in
/// the same list, which gives the same masternode (CJ0 notes D8).
fn resolve_queue_masternode<M: TipMasternodes + ?Sized>(
    mns: &M,
    dsq: &DsQueue,
) -> Result<MixingMasternode, DsqOutcome> {
    if dsq.pro_tx_hash.is_null() {
        return Err(DsqOutcome::Misbehaving(100));
    }
    mns.valid_mn(&dsq.pro_tx_hash).ok_or(DsqOutcome::Misbehaving(10))
}

/// Shared tail of both sides: bounds, then the signature. `Err` is the outcome to return.
fn check_queue_bounds_and_sig(
    entry: &QueueEntry,
    mn: &MixingMasternode,
    now: i64,
) -> Result<(), DsqOutcome> {
    if entry.is_time_out_of_bounds(now) {
        return Err(DsqOutcome::Ignored);
    }
    match check_queue_signature(&entry.dsq, &mn.operator_key) {
        BlsVerdict::Valid => Ok(()),
        BlsVerdict::Invalid => Err(DsqOutcome::Misbehaving(10)),
        BlsVerdict::Unsupported => Err(DsqOutcome::Ignored),
    }
}

// Core: coinjoin/server.cpp:129 CCoinJoinServer::ProcessDSQUEUE
/// A masternode's handling of a received queue, at the adjusted time `now`, into the session's
/// queue manager ([`ServerSession::process_dsq`](crate::server::ServerSession::process_dsq)).
pub(crate) fn server_process_dsq<M: TipMasternodes + ?Sized>(
    queues: &mut QueueManager,
    counters: &mut MixingCounters,
    mns: &M,
    dsq: DsQueue,
    now: i64,
) -> DsqOutcome {
    if !denom::is_valid_denomination(dsq.denom) {
        return DsqOutcome::Misbehaving(10);
    }
    let mn = match resolve_queue_masternode(mns, &dsq) {
        Ok(mn) => mn,
        Err(outcome) => return outcome,
    };
    let entry = QueueEntry::new(dsq, mn.collateral);
    if queues.is_duplicate(&entry) {
        return DsqOutcome::Ignored;
    }
    if let Err(outcome) = check_queue_bounds_and_sig(&entry, &mn, now) {
        return outcome;
    }
    if entry.is_ready() {
        return DsqOutcome::Ignored;
    }
    // Don't let a few masternodes dominate the queues.
    if counters.is_mixing_threshold_exceeded(&mn.pro_tx_hash, mns.enabled_count()) {
        return DsqOutcome::Ignored;
    }
    counters.allow_mixing(&mn.pro_tx_hash);
    let hash = entry.hash();
    queues.add(entry);
    DsqOutcome::Relay(hash)
}

/// The client's mixing sessions, across every wallet (Core's `ForAnyCJClientMan`).
pub trait ClientQueueSessions {
    /// `TrySubmitDenominate(pro_tx_hash)`: a session mixing on that masternode and waiting in
    /// [`PoolState::Queue`](crate::pool::PoolState::Queue) submits its entry; whether one did.
    fn try_submit_denominate(&mut self, pro_tx_hash: &TxHash) -> bool;

    /// Whether a session mixes on the masternode with this collateral
    /// (`MarkAlreadyJoinedQueueAsTried`).
    fn is_mixing_on(&self, collateral: &OutPoint) -> bool;
}

// Core: coinjoin/walletman.cpp:243 CJWalletManagerImpl::ProcessDSQueue
/// A client's handling of a received queue, at the adjusted time `now`.
pub fn client_process_dsq<M: TipMasternodes + ?Sized, S: ClientQueueSessions + ?Sized>(
    queues: &mut QueueManager,
    counters: &mut MixingCounters,
    mns: &M,
    sessions: &mut S,
    dsq: DsQueue,
    now: i64,
) -> DsqOutcome {
    let mn = match resolve_queue_masternode(mns, &dsq) {
        Ok(mn) => mn,
        Err(outcome) => return outcome,
    };
    let mut entry = QueueEntry::new(dsq, mn.collateral);
    // No way the same masternode sends another queue with the same readiness this soon.
    if queues.has_queue(&entry.hash())
        || queues.has_queue_from_masternode(&entry.masternode_outpoint, entry.is_ready())
    {
        return DsqOutcome::Ignored;
    }
    if let Err(outcome) = check_queue_bounds_and_sig(&entry, &mn, now) {
        return outcome;
    }
    if entry.is_ready() && sessions.try_submit_denominate(&mn.pro_tx_hash) {
        return DsqOutcome::Submitted;
    }
    if counters.is_mixing_threshold_exceeded(&mn.pro_tx_hash, mns.enabled_count()) {
        return DsqOutcome::Ignored;
    }
    counters.allow_mixing(&mn.pro_tx_hash);
    if sessions.is_mixing_on(&entry.masternode_outpoint) {
        entry.tried = true;
    }
    queues.add(entry);
    DsqOutcome::Added
}
