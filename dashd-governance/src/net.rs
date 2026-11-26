//! Core's `NetGovernance` on the network: the `govsync`, `govobj` and `govobjvote` handlers, and
//! `AlreadyHave` and `ProcessGetData` for the two governance inventory types, as P2a-core's
//! registry handler.
//!
//! Admission is a V command, which no holder of a header cell may wait on (P3 R12): `govobj`
//! gates, reads and consumes its announcement in the peer region, waits for the admission as a
//! continuation outside it, applies its effects and forgets the request when V answers, and
//! scores in the reopened region. Votes take the governance lock alone and finish in the region.
//!
//! Relays are queued as Core's `RelayObject` and `RelayVote` queue them, and the scheduler's
//! flush announces them ([`crate::schedule`]). Relay order across the admission and vote routes
//! is not lock order.

use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};

use dash_dml::MnList;
use dash_governance::manager::{Effects, Inventory as GovInv, ObjectOutcome};
use dash_governance::strings::hash_hex;
use dash_governance::superblocks::HeightOverflow;
use dash_governance::{object, vote};
use dash_num::Hash256;
use dash_p2p::policy::{GovSyncRead, Throw};
use dash_p2p::{CommandString, OutMsg, PeerHandle};
use dash_p2p_core::{GovSync, Inv, InvType, Inventory, Ssc};
use dash_primitives::{GovObject, GovVote};
use dash_types::Numeric as _;
use dash_types::codec::BaseCodec;
use dashd_masternode::netfulfilled::NetFulfilledRequestManager;
use dashd_masternode::sync::{MASTERNODE_SYNC_GOVOBJ, MASTERNODE_SYNC_GOVOBJ_VOTE};
use dashd_masternode::sync_manager::empty_bloom_filter;
use dashd_p2p::processing::bloom::BloomFilter;
use dashd_p2p::processing::logic::out_msg;
use dashd_p2p::processing::peer::assert_no_header_cell;
use dashd_p2p::processing::registry::CoreHandler;
use dashd_p2p::processing::seams::Interrupted;
use dashd_p2p::{HandlerCx, HandlerResult, NetHandler, PeerLogic, PostRegion};

use crate::exec::{EffectsSink, GovExec, GovReply};
use crate::handle::{Admitted, GovernanceHandle};
use crate::sync::DmnTip;

const COMMANDS: [CommandString; 3] =
    [CommandString::GOVSYNC, CommandString::GOVOBJ, CommandString::GOVOBJVOTE];

/// The governance manager's and mnsync's state the handler reads.
pub trait GovSyncState: Send + Sync {
    /// `CGovernanceManager::IsValid()`.
    fn is_valid(&self) -> bool;
    /// `CMasternodeSync::IsBlockchainSynced()`.
    fn is_blockchain_synced(&self) -> bool;
    /// `CMasternodeSync::IsSynced()`.
    fn is_synced(&self) -> bool;
    /// `CMasternodeSync::BumpAssetLastTime(func)`.
    fn bump_asset_last_time(&self, func: &str);
}

/// The peer manager's sends that `HandlerCx` does not carry, and the scheduler's. dashd-node
/// implements it over its processor's shared half (S5).
pub trait GovPeers: Send + Sync {
    /// `PeerPushInventory(peer, inv)`: queued for that peer's next inventory trickle.
    fn push_inventory(&self, peer: i64, inv: &Inventory);
    /// `PeerRelayInv(inv)`.
    fn relay_inv(&self, inv: &Inventory);
    /// `PeerForgetObjectRequest(inv)`: every peer's announcement of `inv` dropped.
    fn forget_object_request(&self, inv: &Inventory);
    /// The fully connected peers that `CanRelay()`, in the network core's order.
    fn relay_peers(&self) -> Vec<i64>;
    /// `PushMessage(peer, msg)`.
    fn push(&self, peer: i64, msg: OutMsg);
}

/// Core's `m_relay_invs`: what `RelayObject` and `RelayVote` queued, until the scheduler's
/// flush takes it.
#[derive(Debug, Default)]
pub struct RelayInvs(Mutex<Vec<Inventory>>);

impl RelayInvs {
    /// Queues `effects`' objects, then its votes, as the manager queued them.
    pub fn queue(&self, effects: &Effects) {
        let objects = effects.relay_objects.iter().map(|h| object_inv(*h));
        let votes = effects.relay_votes.iter().map(|h| vote_inv(*h));
        self.lock().extend(objects.chain(votes));
    }

    pub fn push(&self, inv: Inventory) {
        self.lock().push(inv);
    }

    /// `FetchRelayInventory()`.
    // Core: governance/governance.cpp v24.0.2:519-525
    pub fn take(&self) -> Vec<Inventory> {
        std::mem::take(&mut *self.lock())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Inventory>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// `CInv(MSG_GOVERNANCE_OBJECT, hash)`.
pub fn object_inv(hash: [u8; 32]) -> Inventory {
    Inventory::new(InvType::GovernanceObject, Hash256::from_lendian(hash))
}

/// `CInv(MSG_GOVERNANCE_OBJECT_VOTE, hash)`.
pub fn vote_inv(hash: [u8; 32]) -> Inventory {
    Inventory::new(InvType::GovernanceObjectVote, Hash256::from_lendian(hash))
}

/// `govsync` for `hash` with an empty filter: the object, not its votes.
pub(crate) fn object_request(hash: [u8; 32]) -> OutMsg {
    out_msg(
        CommandString::GOVSYNC,
        &GovSync::new(Hash256::from_lendian(hash), empty_bloom_filter()),
    )
}

/// What the handler and its continuations share.
pub(crate) struct Shared<E> {
    pub(crate) handle: Arc<GovernanceHandle<E>>,
    pub(crate) sync: Arc<dyn GovSyncState>,
    pub(crate) dmn: Arc<dyn DmnTip>,
    pub(crate) fulfilled: Arc<NetFulfilledRequestManager>,
    pub(crate) peers: Arc<dyn GovPeers>,
    pub(crate) relay: Arc<RelayInvs>,
    pub(crate) sink: Arc<dyn EffectsSink>,
}

impl<E: GovExec + 'static> Shared<E> {
    /// Applies a manager call's effects: relays queued, sync bumps, then the rest (meta) to the
    /// effects worker. The caller does the request forgets Core's handler does.
    pub(crate) fn apply(&self, mut effects: Effects) {
        self.relay.queue(&effects);
        for func in &effects.sync_bumps {
            self.sync.bump_asset_last_time(func);
        }
        effects.relay_objects.clear();
        effects.relay_votes.clear();
        effects.sync_bumps.clear();
        if effects != Effects::default() {
            self.sink.submit(effects);
        }
    }
}

/// Core's `NetGovernance`, registered only while governance is enabled.
pub struct NetGovernance<E>(Arc<Shared<E>>);

impl<E: GovExec + 'static> NetGovernance<E> {
    /// `relay` is the queue the scheduler's flush drains; `sink` takes the effects the handler
    /// does not apply itself.
    pub fn new(
        handle: Arc<GovernanceHandle<E>>,
        sync: Arc<dyn GovSyncState>,
        dmn: Arc<dyn DmnTip>,
        fulfilled: Arc<NetFulfilledRequestManager>,
        peers: Arc<dyn GovPeers>,
        relay: Arc<RelayInvs>,
        sink: Arc<dyn EffectsSink>,
    ) -> Self {
        Self(Arc::new(Shared {
            handle,
            sync,
            dmn,
            fulfilled,
            peers,
            relay,
            sink,
        }))
    }

    pub(crate) fn shared(&self) -> &Arc<Shared<E>> {
        &self.0
    }

    // Core: governance/net_governance.cpp v24.0.2:86-169
    fn govsync(&self, cx: &mut HandlerCx<'_>, payload: &[u8]) -> Result<(), Throw> {
        let s = &*self.0;
        if !s.sync.is_synced() {
            return Ok(());
        }
        let id = cx.id();
        let mut data = payload;
        let read = match cx.read::<GovSyncRead>(&mut data) {
            Ok(read) => read,
            // `nProp` is read outside the handler's catch.
            Err(e) if payload.len() < 32 => return Err(e),
            Err(e) => {
                let msg = format!("misformatted govsync bloom filter. peer={id} error={}", e.what);
                cx.misbehaving(100, &msg);
                return Ok(());
            }
        };
        let filter = read.sync.bloom_filter();
        let bloom = BloomFilter::from(filter.clone());
        if !bloom.is_within_size_constraints() {
            cx.misbehaving(100, "");
            return Ok(());
        }
        tracing::debug!(target: "gobject", "MNGOVERNANCESYNC -- syncing governance objects to our peer peer={id}");
        let prop = read.sync.object_hash().to_lendian();
        let full_sync = prop == [0; 32];
        let object_fetch = !full_sync && filter.data().is_empty();
        let track =
            full_sync || (!object_fetch && s.handle.lock().have_syncable_object_for_hash(&prop));
        let request = if full_sync {
            "govsync".to_owned()
        } else {
            format!("govsync-votes-{}", hash_hex(&prop))
        };
        let addr = cx.peer.addr().service.clone();
        if track && s.fulfilled.has_fulfilled_request(&addr, &request) {
            tracing::debug!(target: "gobject", "MNGOVERNANCESYNC -- peer already asked me for {}", if full_sync { "the list".to_owned() } else { format!("votes for {}", hash_hex(&prop)) });
            cx.misbehaving(20, "");
            return Ok(());
        }
        if track {
            s.fulfilled.add_fulfilled_request(&addr, &request);
        }
        if object_fetch {
            if s.handle.lock().have_object_for_fetch(&prop) {
                cx.push(out_msg(CommandString::INV, &Inv::new(vec![object_inv(prop)])));
            }
            return Ok(());
        }
        let (item, invs) = if full_sync {
            let invs: Vec<_> = s.handle.lock().syncable_object_invs();
            tracing::debug!(target: "gobject", "MNGOVERNANCESYNC -- syncing {} objects to peer={id}", invs.len());
            (MASTERNODE_SYNC_GOVOBJ, invs.into_iter().map(object_inv).collect::<Vec<_>>())
        } else {
            let invs = self.syncable_vote_invs(&prop, &bloom);
            tracing::debug!(target: "gobject", "MNGOVERNANCESYNC -- syncing {} votes for {} to peer={id}", invs.len(), hash_hex(&prop));
            (MASTERNODE_SYNC_GOVOBJ_VOTE, invs.into_iter().map(vote_inv).collect())
        };
        let count = i32::try_from(invs.len()).unwrap_or(i32::MAX);
        cx.push(out_msg(CommandString::SSC, &Ssc::new(item, count)));
        // dashpay/dash#8112: the requester alone.
        for inv in &invs {
            s.peers.push_inventory(id, inv);
        }
        Ok(())
    }

    /// `GetSyncableVoteInvs(nProp, filter)`: the votes the filter lacks that are valid against
    /// the DMN-tip list, in the vote file's order. The votes and their memoised verdicts are read
    /// under the governance lock; the signatures no verdict covers are checked after it is
    /// released, and their verdicts stored.
    // Core: governance/governance.cpp v24.0.2:666-700; governance/vote.cpp v24.0.2:164-207
    fn syncable_vote_invs(&self, parent: &[u8; 32], filter: &BloomFilter) -> Vec<[u8; 32]> {
        let s = &*self.0;
        let list = s.dmn.dmn_tip_list();
        let adjusted = s.handle.env().clock().adjusted;
        let checks: Vec<_> = {
            let m = s.handle.lock();
            let Some(state) = m.object(parent).filter(|st| !st.cached_delete && !st.expired) else {
                return Vec::new();
            };
            let proposal = state.obj_type == object::PROPOSAL;
            state
                .file
                .votes()
                .filter_map(|v| {
                    let hash = vote::hash(v);
                    if filter.contains(&hash) {
                        return None;
                    }
                    let voting_key = proposal && vote::signal(v) == vote::SIGNAL_FUNDING;
                    let check = match vote::signature_key(v, &list, voting_key, adjusted) {
                        None => Check::Known(false),
                        Some(key) => match state.file.memo(&hash).and_then(|m| m.lookup(&key, v)) {
                            Some(valid) => Check::Known(valid),
                            None => Check::Verify(key, v.clone()),
                        },
                    };
                    Some((hash, check))
                })
                .collect()
        };
        let mut fresh = Vec::new();
        let invs = checks
            .into_iter()
            .filter_map(|(hash, check)| {
                let valid = match check {
                    Check::Known(valid) => valid,
                    Check::Verify(key, v) => {
                        #[cfg(test)]
                        crate::tests::note(crate::tests::Event::Verify);
                        let valid = key.verify(&v, s.handle.params());
                        fresh.push((hash, key, v, valid));
                        valid
                    }
                };
                valid.then_some(hash)
            })
            .collect();
        if !fresh.is_empty() {
            s.handle.lock().memoise_votes(parent, fresh);
        }
        invs
    }

    // Core: governance/net_governance.cpp v24.0.2:171-191
    fn govobj(&self, cx: &mut HandlerCx<'_>, payload: &[u8]) -> Result<HandlerResult, Throw> {
        // Core's read throws to the outer catch, `vchSig` over 96 bytes included.
        // Core: governance/common.h v24.0.2:85
        let mut data = payload;
        let obj: GovObject = cx.read(&mut data)?;
        let hash = object::hash(&obj);
        tracing::debug!(target: "gobject", "MNGOVERNANCEOBJECT -- Received object: {}", hash_hex(&hash));
        if !cx.consume_object_request(&object_inv(hash)) {
            tracing::debug!(target: "gobject", "MNGOVERNANCEOBJECT -- Received unrequested object: {}", hash_hex(&hash));
            return Ok(HandlerResult::Done);
        }
        Ok(HandlerResult::Continue(Box::new(Admission {
            shared: Arc::clone(&self.0),
            obj: Some(obj),
            hash,
            reply: None,
            score: None,
        })))
    }

    // Core: governance/net_governance.cpp v24.0.2:202-270
    fn govobjvote(&self, cx: &mut HandlerCx<'_>, payload: &[u8]) -> Result<(), Throw> {
        let s = &*self.0;
        let id = cx.id();
        let mut data = payload;
        let Ok(vote) = cx.read::<GovVote>(&mut data) else {
            cx.misbehaving(100, "malformed governance vote");
            return Ok(());
        };
        let hash = vote::hash(&vote);
        if !s.sync.is_blockchain_synced() {
            tracing::debug!(target: "gobject", "MNGOVERNANCEOBJECTVOTE -- masternode list not synced");
            return Ok(());
        }
        let list = s.dmn.dmn_tip_list();
        tracing::debug!(target: "gobject", "MNGOVERNANCEOBJECTVOTE -- Received vote: {}", hash_hex(&hash));
        if !cx.consume_object_request(&vote_inv(hash)) {
            tracing::debug!(target: "gobject", "MNGOVERNANCEOBJECTVOTE -- Received unrequested vote object: {}, peer = {id}", hash_hex(&hash));
            return Ok(());
        }
        let (outcome, effects) = s.handle.process_vote(&vote, &list);
        s.apply(effects);
        match outcome.result {
            Ok(true) => {
                tracing::debug!(target: "gobject", "MNGOVERNANCEOBJECTVOTE -- {} new", hash_hex(&hash));
                cx.forget_object_request(&vote_inv(hash));
                s.sync.bump_asset_last_time("MNGOVERNANCEOBJECTVOTE");
                if !s.sync.is_synced() {
                    tracing::debug!(target: "gobject", "ProcessMessage -- won't relay until fully synced");
                    return Ok(());
                }
                if in_list(&list, &vote) {
                    self.relay_vote(&vote, hash);
                }
            }
            result => {
                let error = result.as_ref().err();
                tracing::debug!(target: "gobject", "MNGOVERNANCEOBJECTVOTE -- Rejected vote, error = {}", error.map_or("", |e| e.message()));
                if let Some(parent) = outcome.to_request {
                    cx.push(object_request(parent));
                }
                let penalty = error.map_or(0, |e| e.penalty());
                if penalty != 0 && s.sync.is_synced() {
                    cx.misbehaving(penalty, "");
                }
            }
        }
        Ok(())
    }

    /// `RelayVote`: queued only while synced, for a masternode in the list it reads.
    // Core: governance/governance.cpp v24.0.2:126-142
    fn relay_vote(&self, vote: &GovVote, hash: [u8; 32]) {
        let s = &*self.0;
        if s.sync.is_synced() && in_list(&s.dmn.dmn_tip_list(), vote) {
            s.relay.push(vote_inv(hash));
        }
    }
}

/// A stored vote's signature: known from `IsValid`'s earlier checks or its memo, or to verify.
enum Check {
    Known(bool),
    Verify(vote::SigKey, GovVote),
}

fn in_list(list: &MnList, vote: &GovVote) -> bool {
    list.get_by_collateral(&vote.masternode_outpoint()).is_some()
}

impl<E: GovExec + 'static> NetHandler for NetGovernance<E> {
    fn commands(&self) -> &'static [CommandString] {
        &COMMANDS
    }

    fn core_handler(&self) -> Option<CoreHandler> {
        Some(CoreHandler::NetGovernance)
    }

    fn process_message(
        &mut self,
        _cx: &mut HandlerCx<'_>,
        _cmd: &CommandString,
        _payload: &[u8],
    ) -> Result<(), Throw> {
        unreachable!("the registry calls process_message_result")
    }

    // Core: governance/net_governance.cpp v24.0.2:80-84
    fn process_message_result(
        &mut self,
        cx: &mut HandlerCx<'_>,
        cmd: &CommandString,
        payload: &[u8],
    ) -> Result<HandlerResult, Throw> {
        if !self.0.sync.is_valid() || !self.0.sync.is_blockchain_synced() {
            return Ok(HandlerResult::Done);
        }
        match *cmd {
            CommandString::GOVSYNC => self.govsync(cx, payload).map(|()| HandlerResult::Done),
            CommandString::GOVOBJ => self.govobj(cx, payload),
            CommandString::GOVOBJVOTE => self.govobjvote(cx, payload).map(|()| HandlerResult::Done),
            _ => Ok(HandlerResult::Done),
        }
    }

    // Core: governance/net_governance.cpp v24.0.2:273-282
    fn already_have(&self, inv: &Inventory) -> bool {
        let hash = inv.inv_hash().to_lendian();
        let probe = match inv.inv_type() {
            InvType::GovernanceObject => GovInv::Object(hash),
            InvType::GovernanceObjectVote => GovInv::Vote(hash),
            _ => return false,
        };
        !self.0.sync.is_valid() || !self.0.handle.lock().confirm_inventory_request(probe)
    }

    // Core: governance/net_governance.cpp v24.0.2:284-303
    fn process_getdata(&mut self, cx: &mut HandlerCx<'_>, inv: &Inventory) -> bool {
        let hash = inv.inv_hash().to_lendian();
        let mut payload = Vec::new();
        let cmd = {
            let m = self.0.handle.lock();
            match inv.inv_type() {
                InvType::GovernanceObject if m.have_object_for_hash(&hash) => {
                    let Some(obj) = m.object_for_hash(&hash) else {
                        return false;
                    };
                    obj.encode(&mut payload);
                    CommandString::GOVOBJ
                }
                InvType::GovernanceObjectVote if m.have_vote(&hash) => {
                    let Some(v) = m.vote_for_hash(&hash) else {
                        return false;
                    };
                    v.encode(&mut payload);
                    CommandString::GOVOBJVOTE
                }
                _ => return false,
            }
        };
        cx.push(OutMsg::new(cmd, payload));
        true
    }
}

/// A requested object's admission, posted to V on the first poll, outside the region. When V
/// answers, its effects are applied and an admitted object's request is forgotten; the reopened
/// region scores a refusal. When the driver drops the wait first (a disconnect or shutdown), the
/// effects go to the [`EffectsSink`] and the request is still forgotten: only the score is lost.
/// A disconnect before the first poll drops the object before V sees it.
struct Admission<E> {
    shared: Arc<Shared<E>>,
    obj: Option<GovObject>,
    hash: [u8; 32],
    reply: Option<GovReply<Admitted>>,
    score: Option<Score>,
}

/// What the reopened region does with a refusal.
enum Score {
    /// It was the peer's object.
    Peer,
    /// Our index failed on the collateral (DEC-1756).
    IndexFault,
}

/// `ProcessObject` returned true.
fn admitted(outcome: &Result<ObjectOutcome, HeightOverflow>) -> bool {
    matches!(outcome, Ok(o) if o.ok)
}

impl<E: GovExec + 'static> PostRegion for Admission<E> {
    // Core: governance/net_governance.cpp v24.0.2:193-195
    fn poll(
        &mut self,
        _logic: &mut PeerLogic,
        _peer: &PeerHandle,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), Interrupted>> {
        let inv = object_inv(self.hash);
        if let Some(obj) = self.obj.take() {
            assert_no_header_cell();
            let shared = Arc::clone(&self.shared);
            self.reply = Some(self.shared.handle.admit_or(obj, move |outcome| {
                if admitted(outcome) {
                    shared.peers.forget_object_request(&inv);
                }
            }));
        }
        let Some(reply) = self.reply.as_mut() else {
            return Poll::Ready(Ok(()));
        };
        let answer = std::task::ready!(reply.poll_wait(cx));
        self.reply = None;
        if let Some(Admitted {
            outcome,
            effects,
            index_fault,
        }) = answer
        {
            self.shared.apply(effects);
            self.score = if admitted(&outcome) {
                self.shared.peers.forget_object_request(&inv);
                None
            } else if index_fault {
                Some(Score::IndexFault)
            } else {
                Some(Score::Peer)
            };
        }
        Poll::Ready(Ok(()))
    }

    // Core: governance/net_governance.cpp v24.0.2:196-198
    fn tail(self: Box<Self>, cx: &mut HandlerCx<'_>) -> Result<(), Throw> {
        match self.score {
            Some(Score::Peer) => cx.misbehaving(20, ""),
            Some(Score::IndexFault) => {
                tracing::error!(target: "gobject", "MNGOVERNANCEOBJECT -- {} not admitted: the transaction index failed on its collateral; peer={} not scored", hash_hex(&self.hash), cx.id());
            }
            None => {}
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests;
