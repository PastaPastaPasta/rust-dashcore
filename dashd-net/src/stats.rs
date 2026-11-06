//! Core's `CNodeStats` (`CNode::CopyStats` and `CConnman::GetNodeStats`), the network half of
//! `getpeerinfo`; P2a-core's `PeerStateStats` is the processor half.

use std::collections::HashMap;

use dash_p2p::conn::ByteStats;
use dash_p2p::{Address, CommandString, ConnectionType, NetCore, Network, PeerHandle, PeerId};
use dash_p2p::{Permissions, Service};
use dash_p2p_transport::TransportInfo;

/// One peer's `CNodeStats`.
#[derive(Clone, Debug)]
pub struct NodeStats {
    /// `nodeid`.
    pub id: PeerId,
    /// `addr`.
    pub addr: Address,
    /// `addrBind`.
    pub addr_bind: Service,
    /// `m_network` (`ConnectedThroughNetwork`).
    pub network: Network,
    /// `m_last_send`, seconds.
    pub last_send: i64,
    /// `m_last_recv`, seconds.
    pub last_recv: i64,
    /// `m_last_tx_time`, seconds.
    pub last_tx_time: i64,
    /// `m_last_block_time`, seconds.
    pub last_block_time: i64,
    /// `m_connected`, seconds.
    pub connected: i64,
    /// `nTimeOffset`.
    pub time_offset: i64,
    /// `m_addr_name`.
    pub addr_name: String,
    /// `nVersion`.
    pub version: i32,
    /// `cleanSubVer`.
    pub clean_subver: String,
    /// `fInbound`.
    pub inbound: bool,
    /// `m_bip152_highbandwidth_to`.
    pub bip152_highbandwidth_to: bool,
    /// `m_bip152_highbandwidth_from`.
    pub bip152_highbandwidth_from: bool,
    /// `nSendBytes`.
    pub send_bytes: u64,
    /// `nRecvBytes`.
    pub recv_bytes: u64,
    /// `mapSendBytesPerMsgType`.
    pub send_bytes_per_msg: HashMap<CommandString, u64>,
    /// `mapRecvBytesPerMsgType`.
    pub recv_bytes_per_msg: HashMap<CommandString, u64>,
    /// `m_transport_type` and `m_session_id`.
    pub transport: TransportInfo,
    /// `m_permission_flags`.
    pub permissions: Permissions,
    /// `m_last_ping_time`, µs.
    pub last_ping_us: i64,
    /// `m_min_ping_time`, µs.
    pub min_ping_us: i64,
    /// `addrLocal`: empty until the peer reported a valid address.
    pub addr_local: String,
    /// `m_mapped_as`.
    pub mapped_as: u32,
    /// `verifiedProRegTxHash`.
    pub verified_proreg_hash: [u8; 32],
    /// `verifiedPubKeyHash`.
    pub verified_pubkey_hash: [u8; 32],
    /// `m_masternode_connection`.
    pub masternode_connection: bool,
    /// `m_conn_type`.
    pub conn_type: ConnectionType,
}

impl NodeStats {
    /// `CNode::CopyStats` plus the AS mapping.
    // Core: net.cpp:596-646 CNode::CopyStats
    pub fn of(core: &NetCore, p: &PeerHandle) -> Self {
        let ByteStats {
            sent,
            recv,
        } = p.byte_stats();
        Self {
            id: p.id(),
            addr: p.addr().clone(),
            addr_bind: p.addr_bind().clone(),
            network: p.network(),
            last_send: p.last_send(),
            last_recv: p.last_recv(),
            last_tx_time: p.last_tx_time(),
            last_block_time: p.last_block_time(),
            connected: p.connected(),
            time_offset: p.time_offset(),
            addr_name: p.addr_name().to_owned(),
            version: p.version(),
            clean_subver: p.clean_subver(),
            inbound: p.is_inbound(),
            bip152_highbandwidth_to: p.bip152_highbandwidth_to(),
            bip152_highbandwidth_from: p.bip152_highbandwidth_from(),
            send_bytes: p.send_bytes(),
            recv_bytes: p.recv_bytes(),
            send_bytes_per_msg: sent,
            recv_bytes_per_msg: recv,
            transport: p.transport_info(),
            permissions: p.permissions(),
            last_ping_us: p.last_ping_us(),
            min_ping_us: p.min_ping_us(),
            // Leave the string empty if addrLocal is invalid (not filled in yet).
            addr_local: p
                .addr_local()
                .filter(|s| s.addr.is_valid())
                .map(|s| s.to_string())
                .unwrap_or_default(),
            mapped_as: core.netgroup().mapped_as(&p.addr().service.addr),
            verified_proreg_hash: p.verified_proreg_hash(),
            verified_pubkey_hash: p.verified_pubkey_hash(),
            masternode_connection: p.masternode_connection(),
            conn_type: p.conn_type(),
        }
    }
}

/// `CConnman::GetNodeStats`: every connection not marked for disconnection, in table order.
// Core: net.cpp:4563-4576 CConnman::GetNodeStats
pub fn node_stats(core: &NetCore) -> Vec<NodeStats> {
    core.peers()
        .iter()
        .filter(|p| !p.disconnect_requested())
        .map(|p| NodeStats::of(core, p))
        .collect()
}
