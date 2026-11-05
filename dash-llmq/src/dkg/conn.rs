//! The bad-connection inputs of `VerifyConnectionAndMinProtoVersions` (DKG-01, v24; LQ4 §2.6):
//! the sporks that gate it, and the snapshot of the node's connections and masternode metadata
//! that DK1 takes when the Complain phase starts.

use std::collections::BTreeSet;

use dash_evo::types::ProTxHash;

use super::wire::LlmqType;

/// `MIN_MASTERNODE_PROTO_VERSION` (`version.h:23`): a member whose first connection reports less
/// is a bad connection.
pub const MIN_MASTERNODE_PROTO_VERSION: i32 = 70242;

/// `NODE_COMPACT_FILTERS` (`protocol.h:322`).
pub const NODE_COMPACT_FILTERS: u64 = 1 << 6;

/// `EvalSpork(llmqType, spork_value)` (`llmq/options.cpp:26-35`): a value of 0 enables every
/// type; 1 every type but `LLMQ_100_67`, `LLMQ_400_60` and `LLMQ_400_85`; anything else none.
/// `IsQuorumPoseEnabled` evaluates SPORK_23's value, `IsAllMembersConnectedEnabled` SPORK_21's.
pub fn eval_spork(t: LlmqType, spork_value: i64) -> bool {
    match spork_value {
        0 => true,
        1 => !matches!(t, LlmqType::Llmq100_67 | LlmqType::Llmq400_60 | LlmqType::Llmq400_85),
        _ => false,
    }
}

/// One node `CConnman::ForEachNode` visits: Core's fully connected nodes only
/// (`fSuccessfullyConnected && !fDisconnect`, `net.h:1423-1426`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConnFacts {
    /// `GetVerifiedProRegTxHash()`: the null hash when MNAUTH verified none.
    pub verified_pro_tx: ProTxHash,
    /// `nVersion`.
    pub version: i32,
    /// `m_masternode_connection`.
    pub masternode_connection: bool,
    /// `m_version_relay_txs`.
    pub relay_txs: bool,
    /// `m_version_services`.
    pub services: u64,
}

/// What `VerifyConnectionAndMinProtoVersions` reads (`a/dkgsession.cpp:226-290`), taken by DK1
/// when it calls [`super::session::DkgSession::verify_and_complain`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConnSnapshot {
    /// `IsQuorumPoseEnabled(type)`: [`eval_spork`] of SPORK_23. Off, nothing else is read.
    pub pose_enabled: bool,
    /// `IsAllMembersConnectedEnabled(type)`: [`eval_spork`] of SPORK_21.
    pub all_members_connected: bool,
    /// The fully connected nodes, in `m_nodes` order.
    pub nodes: Vec<ConnFacts>,
    /// The members `OutboundFailedTooManyTimes` holds for.
    pub outbound_failed_too_many: BTreeSet<ProTxHash>,
    /// The members `IsPlatformBanned` holds for.
    pub platform_banned: BTreeSet<ProTxHash>,
}

/// What the fold keeps of a proTxHash's connections.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PeerInfo {
    /// The first connection's `nVersion`.
    version: i32,
    has_mn_connection: bool,
    relays_txs: bool,
    serves_compact_filters: bool,
}

/// Why a member is a bad connection, in the order Core checks; each is Core's log line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConnFault {
    /// Not in the fold while all members must be connected.
    NotConnected,
    /// The first connection's version is below [`MIN_MASTERNODE_PROTO_VERSION`].
    MinProtoVersion(i32),
    /// A masternode connection, none relaying transactions.
    NoTxRelay,
    /// A masternode connection, none serving compact block filters.
    NoCompactFilters,
    /// `OutboundFailedTooManyTimes`.
    OutboundFailed,
    /// `IsPlatformBanned`.
    PlatformBanned,
}

impl ConnFault {
    /// The line Core logs for member `who` (`a/dkgsession.cpp:265-288`).
    pub(crate) fn line(self, who: &ProTxHash) -> String {
        match self {
            Self::NotConnected => format!("{who} is not connected to us, badConnection=1"),
            Self::MinProtoVersion(has) => format!(
                "{who} does not have min proto version {MIN_MASTERNODE_PROTO_VERSION} (has {has})"
            ),
            Self::NoTxRelay => format!("{who} does not relay transactions"),
            Self::NoCompactFilters => format!("{who} does not serve compact block filters"),
            Self::OutboundFailed => format!("{who} failed to connect to it too many times"),
            Self::PlatformBanned => format!("{who} is Platform PoSe banned"),
        }
    }
}

impl ConnSnapshot {
    /// The `ForEachNode` fold for `pro_tx`: the first node's version, and over its masternode
    /// connections whether one relays transactions and one serves compact filters. Nodes
    /// without a verified proTxHash are skipped.
    fn fold(&self, pro_tx: &ProTxHash) -> Option<PeerInfo> {
        let mut info: Option<PeerInfo> = None;
        let nodes = self.nodes.iter().filter(|n| !n.verified_pro_tx.is_null());
        for node in nodes.filter(|n| n.verified_pro_tx == *pro_tx) {
            let it = info.get_or_insert(PeerInfo {
                version: node.version,
                ..PeerInfo::default()
            });
            if node.masternode_connection {
                it.has_mn_connection = true;
                it.relays_txs |= node.relay_txs;
                it.serves_compact_filters |= node.services & NODE_COMPACT_FILTERS != 0;
            }
        }
        info
    }

    /// Member `pro_tx`'s faults, in Core's order: the else-if chain over the fold, then the
    /// two metadata checks, each independently. `badConnection` is set iff any is returned.
    pub(crate) fn faults(&self, pro_tx: &ProTxHash) -> Vec<ConnFault> {
        let chain = match self.fold(pro_tx) {
            None if self.all_members_connected => Some(ConnFault::NotConnected),
            None => None,
            Some(it) if it.version < MIN_MASTERNODE_PROTO_VERSION => {
                Some(ConnFault::MinProtoVersion(it.version))
            }
            Some(it) if it.has_mn_connection && !it.relays_txs => Some(ConnFault::NoTxRelay),
            Some(it) if it.has_mn_connection && !it.serves_compact_filters => {
                Some(ConnFault::NoCompactFilters)
            }
            Some(_) => None,
        };
        chain
            .into_iter()
            .chain(
                self.outbound_failed_too_many.contains(pro_tx).then_some(ConnFault::OutboundFailed),
            )
            .chain(self.platform_banned.contains(pro_tx).then_some(ConnFault::PlatformBanned))
            .collect()
    }
}

#[cfg(test)]
impl ConnFacts {
    /// A masternode connection to `who` that passes every check.
    pub(crate) const fn compliant(who: ProTxHash) -> Self {
        Self {
            verified_pro_tx: who,
            version: MIN_MASTERNODE_PROTO_VERSION,
            masternode_connection: true,
            relay_txs: true,
            services: NODE_COMPACT_FILTERS,
        }
    }
}

#[cfg(test)]
impl ConnSnapshot {
    /// PoSe on, every member required, and a compliant connection to each of `members`.
    pub(crate) fn connected(members: impl IntoIterator<Item = ProTxHash>) -> Self {
        Self {
            pose_enabled: true,
            all_members_connected: true,
            nodes: members.into_iter().map(ConnFacts::compliant).collect(),
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests;
