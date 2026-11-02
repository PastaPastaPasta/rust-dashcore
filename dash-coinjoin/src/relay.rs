//! The CoinJoin relay rules of Core's `net_processing.cpp` (v24.0.0-rc.3): who is sent queues
//! and how, and how a mixing transaction is announced. The node's peer manager (P6a) applies them
//! (CJ0 notes D6).

use dash_p2p_core::{DsTx, ProtocolVersion};
use dash_primitives::TxHash;

use crate::dstx::{DstxManager, PoolLimits, can_announce_dstx_to};

/// How a peer wants mixing queues (Core's `Peer::WantsDSQ`); [`WantsDsq::None`] until it sends
/// `senddsq`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WantsDsq {
    /// Not at all.
    #[default]
    None,
    /// Announced by inventory (`MSG_DSQ`).
    Inv,
    /// Every queue, sent as a `dsq` message.
    All,
}

// Core: net_processing.cpp:4485 SENDDSQUEUE
/// The peer's wish after its `senddsq(enabled)` on a connection at the negotiated
/// `common_version`: none when disabled, every queue below
/// [`ProtocolVersion::DSQ_INV`], inventory from it on.
pub fn wants_dsq_after_senddsq(enabled: bool, common_version: ProtocolVersion) -> WantsDsq {
    if !enabled {
        WantsDsq::None
    } else if common_version < ProtocolVersion::DSQ_INV {
        WantsDsq::All
    } else {
        WantsDsq::Inv
    }
}

/// What to send a peer for a queue being relayed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DsqRelay {
    /// Nothing.
    Skip,
    /// Queue `MSG_DSQ` with the queue's hash for the peer's next inventory.
    Inv,
    /// Send the `dsq` message now.
    Message,
}

// Core: net_processing.cpp:2555 PeerManagerImpl::RelayDSQ
/// `RelayDSQ`, per peer.
pub const fn dsq_relay(wants: WantsDsq) -> DsqRelay {
    match wants {
        WantsDsq::None => DsqRelay::Skip,
        WantsDsq::Inv => DsqRelay::Inv,
        WantsDsq::All => DsqRelay::Message,
    }
}

/// The inventory type a mixing transaction is relayed and announced with.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TxInvKind {
    /// `MSG_TX`.
    Tx,
    /// `MSG_DSTX`.
    Dstx,
}

// Core: net_processing.cpp:2652 PeerManagerImpl::_RelayTransaction
/// The type `_RelayTransaction` queues a transaction under: `MSG_DSTX` when it is a kept DSTX.
/// `txid` is the transaction's [`dstx::txid`](crate::dstx::txid).
pub fn relay_inv_kind(dstxes: &DstxManager, txid: &TxHash) -> TxInvKind {
    if dstxes.contains(txid) {
        TxInvKind::Dstx
    } else {
        TxInvKind::Tx
    }
}

// Core: net_processing.cpp:6635 SendMessages (MSG_DSTX announcement)
/// The type the transaction is announced to a peer at `peer_version` with: `MSG_DSTX` for a kept
/// DSTX the peer can read as one ([`can_announce_dstx_to`]), else `MSG_TX`. `txid` is the
/// transaction's [`dstx::txid`](crate::dstx::txid).
pub fn announce_inv_kind(
    dstxes: &DstxManager,
    txid: &TxHash,
    limits: PoolLimits,
    peer_version: ProtocolVersion,
) -> TxInvKind {
    match dstxes.get(txid) {
        Some(d) if can_announce_dstx_to(&d.dstx.tx, limits, peer_version) => TxInvKind::Dstx,
        _ => TxInvKind::Tx,
    }
}

// Core: net_processing.cpp:2976 ProcessGetData (MSG_TX/MSG_DSTX)
/// The DSTX to answer a `getdata` for a transaction the node has with: the kept one for a
/// `MSG_DSTX` request; `None` means a plain `tx` (also for a `MSG_DSTX` request it does not
/// keep). `txid` is the transaction's [`dstx::txid`](crate::dstx::txid).
pub fn dstx_for_getdata<'a>(
    dstxes: &'a DstxManager,
    kind: TxInvKind,
    txid: &TxHash,
) -> Option<&'a DsTx> {
    match kind {
        TxInvKind::Dstx => dstxes.get(txid).map(|d| &d.dstx),
        TxInvKind::Tx => None,
    }
}
