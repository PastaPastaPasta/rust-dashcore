//! Core's non-transaction inventory relay (P5a D10-D12): each peer's known-inventory filter and
//! `vInventoryOtherToSend`, `PushInv`, `RelayInv` and `SendMessages` section 8's non-tx loop.
//!
//! Dash builds every peer's `TxRelay` (np:446), so `GetInvRelay()` is never null: every peer has
//! an [`InvRelay`], in a leaf cell shared by the peer map's slot and the state like `block_inv`.
//! [`relay_inv`] reaches every peer while the actor holds the source's state: state → map → cell.

use std::collections::BTreeSet;
use std::sync::Arc;

use dash_coinjoin::relay::{DsqRelay, WantsDsq as QueueWish, dsq_relay};
use dash_p2p::bloom::RollingBloom;
use dash_p2p::policy;
use dash_p2p::{CommandString, NetCore, PeerId};
use dash_p2p_core::{DsQueue, InvType, Inventory};
use dash_primitives::TxHash;
use dash_types::Hashable;
use dash_types::Numeric;
use parking_lot::Mutex;

use super::eviction::fully_connected;
use super::logic::{PeerMap, out_msg};
use super::peer::WantsDsq;
use super::seams::InvBatch;
use super::txrequest::TxRequestTracker;

/// The size of Core's `m_tx_inventory_known_filter`.
// Core: net_processing.cpp:314
const KNOWN_FILTER_ELEMENTS: u32 = 50_000;
/// Its false-positive rate.
const KNOWN_FILTER_FP_RATE: f64 = 0.000_001;

/// Core's `TxRelay` members under `m_tx_inventory_mutex` that non-transaction relay uses.
#[derive(Debug)]
pub struct InvRelay {
    /// The filter's tweak, `InitializeNode`'s second draw.
    tweak: u32,
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

    /// Core's `PushInv` for a non-tx type (D12).
    // Core: net_processing.cpp:1228-1257 PushInv
    pub fn push_inv(&mut self, peer: PeerId, inv: &Inventory) {
        if inv.inv_type() == InvType::Block {
            tracing::info!("PushInv -- WARNING: using PushInv for BLOCK inv, peer={peer}");
            return;
        }
        debug_assert!(
            !matches!(inv.inv_type(), InvType::Tx | InvType::Dstx),
            "transaction relay is P5's RelayTransaction"
        );
        if self.is_known(&inv.inv_hash().to_lendian()) {
            tracing::debug!(target: "net", "PushInv -- skipping known inv: {inv} peer={peer}");
            return;
        }
        tracing::debug!(target: "net", "PushInv -- adding new inv: {inv} peer={peer}");
        self.other_to_send.push(*inv);
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

// Core: net_processing.cpp:3835-3840 PostProcessMessage
pub(super) fn post_process_inventory(
    peers: &PeerMap,
    requests: Option<&Mutex<TxRequestTracker>>,
    inventory: &[Inventory],
) {
    for inv in inventory {
        if let Some(requests) = requests {
            requests.lock().forget_tx_hash(inv);
        }
        relay_inv(peers, inv);
    }
}
