//! Core's non-transaction inventory relay (P5a D10-D12): each peer's known-inventory filter and
//! `vInventoryOtherToSend`, `PushInv`, `RelayInv` and `SendMessages` section 8's non-tx loop.
//!
//! Dash builds every peer's `TxRelay` (np:446), so `GetInvRelay()` is never null: every peer has
//! an [`InvRelay`], in a leaf cell shared by the peer map's slot and the state like `block_inv`.
//! [`relay_inv`] reaches every peer while the actor holds the source's state: state → map → cell.

use std::sync::Arc;

use dash_p2p::PeerId;
use dash_p2p::bloom::RollingBloom;
use dash_p2p::policy;
use dash_p2p_core::{InvType, Inventory};
use dash_types::Numeric;
use parking_lot::Mutex;

use super::logic::PeerMap;
use super::seams::InvBatch;

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
        if inv.inv_type == InvType::Block {
            tracing::info!("PushInv -- WARNING: using PushInv for BLOCK inv, peer={peer}");
            return;
        }
        debug_assert!(
            !matches!(inv.inv_type, InvType::Tx | InvType::Dstx),
            "transaction relay is P5's RelayTransaction"
        );
        if self.is_known(&inv.hash.to_lendian()) {
            tracing::debug!(target: "net", "PushInv -- skipping known inv: {inv} peer={peer}");
            return;
        }
        tracing::debug!(target: "net", "PushInv -- adding new inv: {inv} peer={peer}");
        self.other_to_send.push(*inv);
    }

    /// Section 8's loop over `vInventoryOtherToSend`: skips what the peer may not get or already
    /// knows, records the rest as known and appends it to `batch`; the queue is emptied.
    /// `block_relay_only` is the peer's `IsBlockRelayOnly()`.
    // Core: net_processing.cpp:6719-6745
    pub fn send_other(
        &mut self,
        batch: &mut InvBatch<'_>,
        relay_txs: bool,
        block_relay_only: bool,
    ) {
        let send_is = relay_txs && !block_relay_only;
        for inv in std::mem::take(&mut self.other_to_send) {
            let cmd = inv.inv_type.command();
            if !relay_txs && cmd.is_some_and(|c| policy::violates_blocks_only(&c)) {
                continue;
            }
            let hash = inv.hash.to_lendian();
            if self.is_known(&hash) {
                continue;
            }
            if !send_is && inv.inv_type == InvType::IsdLock {
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
