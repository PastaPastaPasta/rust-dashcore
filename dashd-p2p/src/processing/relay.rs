//! Inventory relay: each peer's known filter, transaction trickle queue, BIP35 replies,
//! privacy-gated transaction serving and `SendMessages` section 8's shared inventory batch.
//!
//! Dash builds every peer's `TxRelay` (np:446), so `GetInvRelay()` is never null: every peer has
//! an [`InvRelay`], in a leaf cell shared by the peer map's slot and the state like `block_inv`.
//! [`relay_inv`] reaches every peer while the actor holds the source's state: state → map → cell.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use dash_coinjoin::relay::{DsqRelay, WantsDsq as QueueWish, dsq_relay};
use dash_consensus::{CoreSerialize, Tx, TxRef};
use dash_evo::locks::RecoveredSig;
use dash_num::Hash256;
use dash_p2p::bloom::RollingBloom;
use dash_p2p::policy;
use dash_p2p::{CommandString, NetCore, OutMsg, PeerHandle, PeerId};
use dash_p2p_core::{DsQueue, InvType, Inventory};
use dash_primitives::TxHash;
use dash_types::Hashable;
use dash_types::Numeric;
use parking_lot::Mutex;

use super::eviction::fully_connected;
use super::logic::out_msg;
use super::logic::{PeerLogic, PeerMap, PeerSlot};
use super::peer::PeerState;
use super::peer::WantsDsq;
use super::recsig::{
    RecsigPush, peer_reconstructs_islock_from_recsig, proactive_relay, qsigrec_msg, recsig_relay,
};
use super::registry::HandlerCx;
use super::seams::{InvBatch, TxPool};

/// The size of Core's `m_tx_inventory_known_filter`.
// Core: net_processing.cpp:314
const KNOWN_FILTER_ELEMENTS: u32 = 50_000;
/// Its false-positive rate.
const KNOWN_FILTER_FP_RATE: f64 = 0.000_001;

/// Core's `TxRelay` members under `m_tx_inventory_mutex`.
#[derive(Debug)]
pub struct InvRelay {
    /// The filter's tweak, `InitializeNode`'s second draw.
    tweak: u32,
    /// Core's `m_tx_relay` exists (`SetTxRelay`), mirrored from `PeerState::tx_relay` so
    /// transaction relay reads it under this leaf lock alone.
    pub tx_relay_enabled: bool,
    /// `m_next_inv_send_time != 0s`: the first trickle time has been drawn.
    pub started: bool,
    /// `m_tx_inventory_known_filter`, allocated at its first insert (D10).
    known: Option<RollingBloom>,
    /// `vInventoryOtherToSend`.
    other_to_send: Vec<Inventory>,
    /// `TxRelay::m_tx_inventory_to_send`, keyed by stable txid, under the same leaf lock
    /// as the known-inventory filter.
    pub tx_inventory_to_send: BTreeSet<TxHash>,
}

/// A peer's [`InvRelay`] (Core's `m_tx_inventory_mutex`): a leaf lock.
pub type InvRelayCell = Arc<Mutex<InvRelay>>;

impl InvRelay {
    /// An empty relay state whose filter will use `tweak`.
    pub fn new(tweak: u32) -> Self {
        Self {
            tweak,
            tx_relay_enabled: false,
            started: false,
            known: None,
            other_to_send: Vec::new(),
            tx_inventory_to_send: BTreeSet::new(),
        }
    }

    /// Whether the known filter holds `hash`.
    pub fn is_known(&self, hash: &[u8; 32]) -> bool {
        self.known.as_ref().is_some_and(|f| f.contains(hash))
    }

    /// `m_tx_inventory_known_filter.insert(hash)` (Core's `AddKnownInv`, np:1180-1187).
    pub fn add_known(&mut self, hash: &[u8; 32]) {
        let tweak = self.tweak;
        self.known
            .get_or_insert_with(|| {
                RollingBloom::new(KNOWN_FILTER_ELEMENTS, KNOWN_FILTER_FP_RATE, tweak)
            })
            .insert(hash);
    }

    pub(super) fn finalize(&mut self) {
        self.tx_relay_enabled = false;
        self.started = false;
        self.known = None;
        self.other_to_send.clear();
        self.tx_inventory_to_send.clear();
    }

    /// Core v24.0.0 `src/net_processing.cpp:1228-1257`, `PushInv`.
    pub fn push_inv(&mut self, peer: PeerId, inv: &Inventory) {
        if inv.inv_type() == InvType::Block {
            tracing::info!("PushInv -- WARNING: using PushInv for BLOCK inv, peer={peer}");
            return;
        }
        let tx = matches!(inv.inv_type(), InvType::Tx | InvType::Dstx);
        if tx && !self.started {
            return;
        }
        if self.is_known(&inv.inv_hash().to_lendian()) {
            tracing::debug!(target: "net", "PushInv -- skipping known inv: {inv} peer={peer}");
            return;
        }
        tracing::debug!(target: "net", "PushInv -- adding new inv: {inv} peer={peer}");
        if tx {
            self.tx_inventory_to_send.insert(TxHash::from_lendian(inv.inv_hash().to_lendian()));
        } else {
            self.other_to_send.push(*inv);
        }
    }

    /// Section 8's loop over `vInventoryOtherToSend`: skips what the peer may not get or already
    /// knows, records the rest as known and appends it to `batch`; the queue is emptied.
    /// `block_relay_only` is the peer's `IsBlockRelayOnly()`.
    // Core: net_processing.cpp:6744-6766
    pub fn send_other(
        &mut self,
        batch: &mut InvBatch<'_>,
        relay_txs: bool,
        block_relay_only: bool,
    ) {
        let send_is = relay_txs && !block_relay_only;
        for inv in std::mem::take(&mut self.other_to_send) {
            let cmd = inv.inv_type().command();
            if !relay_txs && cmd.is_some_and(|c| policy::violates_blocks_only(&c)) {
                continue;
            }
            let hash = inv.inv_hash().to_lendian();
            if self.is_known(&hash) {
                continue;
            }
            if !send_is && inv.inv_type() == InvType::IsdLock {
                continue;
            }
            self.add_known(&hash);
            batch.push(inv);
        }
    }
}

/// Core's `RelayInv`: `PushInv` to every peer in the map (D11). Peers that connected but whose
/// state is not in the map yet, or has left it, are skipped, as Core's `m_peer_map` skips them.
// Core: net_processing.cpp:2546-2553 PeerManagerImpl::RelayInv
pub fn relay_inv(peers: &PeerMap, inv: &Inventory) {
    for (id, slot) in peers.lock().iter() {
        slot.inv_relay.lock().push_inv(*id, inv);
    }
}

/// The object used to decide whether a filtered relay is relevant to a peer.
pub enum Related<'a> {
    /// Test and update the peer's bloom filter with a full transaction.
    Tx(&'a Tx),
    /// Test the peer's bloom filter with a transaction hash only.
    Hash(TxHash),
}

/// Core's `RelayInvFiltered`, including the recsig reconstruction suppression for ISDLOCK.
// Core: net_processing.cpp:2588-2648
pub fn relay_inv_filtered(core: &NetCore, peers: &PeerMap, inv: &Inventory, related: Related<'_>) {
    for peer in core.peers().iter().filter(|peer| peer.can_relay()) {
        let Some(slot) = peers.lock().get(&peer.id()).cloned() else {
            continue;
        };
        let mut state = slot.state.lock();
        if !state.relay_txs {
            continue;
        }
        let relevant = match related {
            Related::Tx(tx) => {
                state.bloom.as_mut().is_none_or(|filter| filter.is_relevant_and_update(tx))
            }
            Related::Hash(hash) => {
                state.bloom.as_ref().is_none_or(|filter| filter.contains(hash.as_bytes()))
            }
        };
        if !relevant {
            continue;
        }
        if inv.inv_type() == InvType::IsdLock
            && peer_reconstructs_islock_from_recsig(
                state.wants_recsigs,
                &peer.verified_proreg_hash(),
            )
        {
            continue;
        }
        drop(state);
        slot.inv_relay.lock().push_inv(peer.id(), inv);
    }
}

/// Relays a queue according to each peer's negotiated `senddsq` mode.
/// The caller must hold no peer state lock.
// Core v24.0.0: net_processing.cpp:2560-2585.
pub fn relay_dsq(core: &NetCore, peers: &PeerMap, dsq: &DsQueue) {
    let inv = Inventory::new(InvType::Dsq, dsq.hash());
    let slots: Vec<_> = peers.lock().iter().map(|(id, slot)| (*id, slot.clone())).collect();
    let mut send_all = Vec::new();
    for (id, slot) in slots {
        let wants = match slot.state.lock().wants_dsq {
            WantsDsq::None => QueueWish::None,
            WantsDsq::Inv => QueueWish::Inv,
            WantsDsq::All => QueueWish::All,
        };
        match dsq_relay(wants) {
            DsqRelay::Skip => {}
            DsqRelay::Inv => slot.inv_relay.lock().push_inv(id, &inv),
            DsqRelay::Message => send_all.push(id),
        }
    }
    for id in send_all {
        if let Some(peer) = core.peer(id).filter(fully_connected) {
            core.push(&peer, out_msg(CommandString::from_static("dsq"), dsq));
        }
    }
}

/// Core's `RelayRecoveredSig`: a `qsigrec` to every fully connected subscriber when proactive,
/// else the inventory on every subscriber's relay. The caller must hold no peer state lock.
// Core: v24.0.2 src/net_processing.cpp:2668-2689 PeerManagerImpl::RelayRecoveredSig
pub fn relay_recovered_sig(core: &NetCore, peers: &PeerMap, sig: &RecoveredSig, proactive: bool) {
    let proactive = proactive_relay(sig, proactive);
    let slots: BTreeMap<PeerId, PeerSlot> =
        peers.lock().iter().map(|(id, slot)| (*id, slot.clone())).collect();
    let facts: Vec<_> =
        slots.iter().map(|(id, slot)| (*id, slot.state.lock().wants_recsigs)).collect();
    let inv = Inventory::new(InvType::QuorumRecoveredSig, Hash256::from_lendian(sig.hash()));
    for (id, push) in recsig_relay(facts, proactive) {
        match push {
            RecsigPush::QsigRec => {
                if let Some(peer) = core.peer(id).filter(fully_connected) {
                    core.push(&peer, qsigrec_msg(sig));
                }
            }
            RecsigPush::Inv => slots[&id].inv_relay.lock().push_inv(id, &inv),
        }
    }
}

/// Applies inventory effects from a subsystem that has no sending peer.
pub(super) fn post_process_inventory(
    peers: &PeerMap,
    requests: Option<&Mutex<super::txrequest::TxRequestTracker>>,
    inventory: &[Inventory],
) {
    for inv in inventory {
        if let Some(requests) = requests {
            requests.lock().forget_tx_hash(inv);
        }
        relay_inv(peers, inv);
    }
}

/// The minimum pool age for serving a transaction without an announcement.
pub const UNCONDITIONAL_RELAY_DELAY_S: i64 = 120;
const RELAY_TX_CACHE_TIME_US: i64 = 15 * 60 * 1_000_000;

/// Recently announced transactions retained after pool removal.
#[derive(Default)]
pub(super) struct TxRelayCache {
    transactions: BTreeMap<TxHash, TxRef>,
    expiration: VecDeque<(i64, TxHash)>,
}

impl TxRelayCache {
    fn insert(&mut self, hash: TxHash, tx: TxRef, now_us: i64) {
        while self.expiration.front().is_some_and(|(time, _)| *time < now_us) {
            if let Some((_, expired)) = self.expiration.pop_front() {
                self.transactions.remove(&expired);
            }
        }
        if let std::collections::btree_map::Entry::Vacant(entry) = self.transactions.entry(hash) {
            entry.insert(tx);
            self.expiration.push_back((now_us + RELAY_TX_CACHE_TIME_US, hash));
        }
    }
}

/// Core v24.0.0 `src/net_processing.cpp:3626-3640`, `GetTxAnnouncement`.
fn tx_announcement(tx: &Tx, version: i32) -> Inventory {
    let (kind, hash) = if tx.is_asset_unlock_with_stable_txid() && version >= 70242 {
        (InvType::AssetUnlock, tx.instance_hash())
    } else {
        (InvType::Tx, tx.txid())
    };
    Inventory::new(kind, Hash256::from_lendian(hash.to_lendian()))
}

struct TxAnnouncements<'a> {
    heap: Vec<TxHash>,
    pool: &'a dyn TxPool,
}

impl<'a> TxAnnouncements<'a> {
    fn new(heap: Vec<TxHash>, pool: &'a dyn TxPool) -> Self {
        let mut result = Self {
            heap,
            pool,
        };
        for i in (0..result.heap.len() / 2).rev() {
            result.sift_down(i);
        }
        result
    }

    fn sift_down(&mut self, mut parent: usize) {
        loop {
            let mut child = parent * 2 + 1;
            if child >= self.heap.len() {
                break;
            }
            if child + 1 < self.heap.len()
                && self
                    .pool
                    .compare_depth_and_score(&self.heap[child + 1], &self.heap[child])
                    .is_lt()
            {
                child += 1;
            }
            if !self.pool.compare_depth_and_score(&self.heap[child], &self.heap[parent]).is_lt() {
                break;
            }
            self.heap.swap(parent, child);
            parent = child;
        }
    }

    fn pop(&mut self) -> Option<TxHash> {
        if self.heap.is_empty() {
            return None;
        }
        let result = self.heap.swap_remove(0);
        self.sift_down(0);
        Some(result)
    }
}

impl PeerLogic {
    /// Core v24.0.0 `src/net_processing.cpp:2650-2666`, `RelayTransaction`.
    pub fn relay_transaction(&self, txid: TxHash) {
        let kind = if self.seams.coinjoin.dstx_by_hash(&txid).is_some() {
            InvType::Dstx
        } else {
            InvType::Tx
        };
        let inv = Inventory::new(kind, Hash256::from_lendian(txid.to_lendian()));
        for (id, slot) in self.peers.lock().iter() {
            let mut relay = slot.inv_relay.lock();
            if relay.tx_relay_enabled {
                relay.push_inv(*id, &inv);
            }
        }
    }

    /// Core v24.0.0 `src/net_processing.cpp:2478-2518`, `AskPeersForTransaction`.
    pub fn ask_peers_for_transaction(&self, txid: TxHash) {
        let inv = Inventory::new(InvType::Tx, Hash256::from_lendian(txid.to_lendian()));
        let peers = self.peers.lock();
        let mut tracker = self.object_request.lock();
        for (id, _) in peers
            .iter()
            .filter(|(_, slot)| {
                let relay = slot.inv_relay.lock();
                relay.is_known(&txid.to_lendian())
            })
            .take(4)
        {
            tracker.received_inv(*id, &inv, true, self.core.clock().node_now_micros());
        }
    }

    /// Forget every transaction inventory variant for a stable txid and its current instance.
    pub fn forget_tx(&self, txid: TxHash) {
        match self.seams.txpool.info(&txid) {
            Some(info) => self.forget_transaction(&info.tx),
            None => self.forget_transaction_hash(txid),
        }
    }

    /// Core v24.0.0 `src/net_processing.cpp:1767-1786`, `ReattemptInitialBroadcast`.
    pub fn reattempt_initial_broadcast(&self) {
        for txid in self.seams.txpool.unbroadcast() {
            if self.seams.txpool.exists(&txid) {
                self.relay_transaction(txid);
            } else {
                self.seams.txpool.remove_unbroadcast(&txid, true);
            }
        }
    }

    /// Core v24.0.0 `src/net_processing.cpp:3611-3624`, `CanAnnounceDstxTo`.
    fn announcement(&self, tx: &Tx, peer: &PeerHandle) -> Inventory {
        let inv = tx_announcement(tx, peer.common_version().get());
        if self.seams.coinjoin.dstx_by_hash(&tx.txid()).is_some()
            && ((tx.raw().inputs().len() == tx.raw().outputs().len()
                && tx.raw().inputs().len()
                    <= self.core.config().params.pool_max_participants as usize * 9)
                || peer.common_version().get() >= 70241)
        {
            Inventory::new(InvType::Dstx, inv.inv_hash())
        } else {
            inv
        }
    }

    /// Core v24.0.0 `src/net_processing.cpp:2905-2938`, `FindTxForGetData`.
    fn find_tx_for_getdata(&self, st: &PeerState, hash: &TxHash, now_s: i64) -> Option<TxRef> {
        let pool = &self.seams.txpool;
        let info = pool
            .info(hash)
            .or_else(|| pool.asset_unlock_by_instance_hash(hash).and_then(|txid| pool.info(&txid)));
        if let Some(info) = &info
            && ((st.last_mempool_req != 0 && info.time_s <= st.last_mempool_req)
                || info.time_s <= now_s - UNCONDITIONAL_RELAY_DELAY_S)
        {
            return Some(Arc::clone(&info.tx));
        }
        if !st.recently_announced_invs.contains(&hash.to_lendian()) {
            return None;
        }
        info.map(|info| info.tx).or_else(|| self.tx_relay_cache.transactions.get(hash).cloned())
    }

    /// Core v24.0.0 `src/net_processing.cpp:2970-3020`, transaction `ProcessGetData` branch.
    pub(super) fn process_tx_getdata(&self, cx: &mut HandlerCx<'_>, inv: &Inventory) -> bool {
        let hash = TxHash::from_lendian(inv.inv_hash().to_lendian());
        let now_s = cx.core.clock().node_now().0;
        let Some(tx) = self.find_tx_for_getdata(cx.state, &hash, now_s) else {
            return false;
        };
        let dstx = (inv.inv_type() == InvType::Dstx)
            .then(|| self.seams.coinjoin.dstx_by_hash(&hash))
            .flatten();
        if let Some(payload) = dstx {
            cx.push(OutMsg::new(CommandString::DSTX, payload));
        } else {
            cx.push(OutMsg::new(CommandString::TX, tx.core_serialize()));
        }
        self.seams.txpool.remove_unbroadcast(&tx.txid(), false);
        let parents =
            self.seams.txpool.recent_parents(&tx.txid(), now_s - UNCONDITIONAL_RELAY_DELAY_S);
        let relay = cx.state.inv_relay.lock();
        for parent in parents {
            if !relay.is_known(&parent.to_lendian()) {
                cx.state.recently_announced_invs.insert(&parent.to_lendian());
            }
        }
        true
    }

    /// Core v24.0.0 `src/net_processing.cpp:6619-6741`, BIP35 and transaction trickle.
    pub(super) fn send_transactions(
        &mut self,
        st: &mut PeerState,
        batch: &mut InvBatch<'_>,
        now_us: i64,
        bls: dash_consensus::BlsCtx,
    ) {
        let peer = batch.peer();
        if !st.relay_txs {
            st.inv_relay.lock().tx_inventory_to_send.clear();
        }
        if st.send_mempool {
            let entries = self.seams.txpool.info_all();
            st.send_mempool = false;
            for info in entries {
                st.inv_relay.lock().tx_inventory_to_send.remove(&info.tx.txid());
                if st.bloom.as_mut().is_some_and(|filter| !filter.is_relevant_and_update(&info.tx))
                {
                    continue;
                }
                let inv = self.announcement(&info.tx, peer);
                st.inv_relay.lock().add_known(&inv.inv_hash().to_lendian());
                batch.push(inv);
                if let Some(hash) = self.seams.islocks.islock_hash_by_txid(&info.tx.txid()) {
                    st.inv_relay.lock().add_known(&hash);
                    if !st.wants_recsigs || peer.verified_proreg_hash() == [0; 32] {
                        batch.push(Inventory::new(InvType::IsdLock, Hash256::from_lendian(hash)));
                    }
                }
            }
            if let Some(clsig) = self.seams.chainlocks.best_chainlock(bls)
                && let Some(hash) = dash_evo::locks::clsig_hash(
                    &clsig,
                    dash_evo::locks::ClsigHashSchemes::both(bls),
                )
            {
                st.inv_relay.lock().add_known(&hash);
                batch.push(Inventory::new(InvType::ClSig, Hash256::from_lendian(hash)));
            }
            st.last_mempool_req = now_us / 1_000_000;
        }
        let candidates = st.inv_relay.lock().tx_inventory_to_send.iter().copied().collect();
        let pool = &*self.seams.txpool;
        let mut candidates = TxAnnouncements::new(candidates, pool);
        let broadcast_max = (280 + candidates.heap.len() / 1000 * 5).min(1000);
        let mut relayed = 0;
        while let Some(txid) = candidates.pop() {
            if relayed == broadcast_max {
                break;
            }
            st.inv_relay.lock().tx_inventory_to_send.remove(&txid);
            let Some(info) = pool.info(&txid) else {
                continue;
            };
            let inv = self.announcement(&info.tx, peer);
            if st.inv_relay.lock().is_known(&inv.inv_hash().to_lendian())
                || st.bloom.as_mut().is_some_and(|filter| !filter.is_relevant_and_update(&info.tx))
            {
                continue;
            }
            let hash = TxHash::from_lendian(inv.inv_hash().to_lendian());
            st.recently_announced_invs.insert(&hash.to_lendian());
            self.tx_relay_cache.insert(hash, info.tx, now_us);
            st.inv_relay.lock().add_known(&hash.to_lendian());
            batch.push(inv);
            relayed += 1;
        }
    }
}
