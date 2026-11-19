//! Pure masternode connection maintenance.

use dash_netaddr::Service;
use dash_p2p::PeerId;

use super::QuorumNodes;

const PROBE_WAIT_INTERVAL: i64 = 5;

/// The AllNodes fields read by Core's `CMasternodeUtils::DoMaintenance`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaintenancePeer {
    pub id: PeerId,
    pub inbound: bool,
    pub feeler: bool,
    pub manual: bool,
    pub mn_conn: bool,
    pub probe: bool,
    pub verified: Option<[u8; 32]>,
    pub connected_at: i64,
    pub qwatch: bool,
    pub addr: Service,
}

/// The pure result of one maintenance pass.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MaintenanceResult {
    pub disconnect: Vec<PeerId>,
    pub log: Vec<String>,
}

/// Applies Core v24.0.0's masternode connection maintenance branches in AllNodes order.
// Core: masternode/utils.cpp:16-83
#[allow(clippy::too_many_arguments)]
pub fn do_maintenance(
    synced: bool,
    shutdown: bool,
    peers: &[MaintenancePeer],
    max_outbound: usize,
    now: i64,
    registry: &QuorumNodes,
    mixing_addrs: &[Service],
    log_ips: bool,
) -> MaintenanceResult {
    if !synced || shutdown {
        return MaintenanceResult::default();
    }

    // Core: masternode/utils.cpp:22-38. Unverified MN connections count as outbound slots here.
    let non_masternode_count = peers
        .iter()
        .filter(|peer| {
            (!peer.inbound && !peer.feeler && !peer.manual && !peer.mn_conn && !peer.probe)
                || !is_verified(peer.verified)
        })
        .count();
    if non_masternode_count < max_outbound {
        return MaintenanceResult::default();
    }

    let mut result = MaintenanceResult::default();
    for peer in peers {
        if peer.probe {
            // Core: net.h:70; masternode/utils.cpp:42-45.
            if now - peer.connected_at < PROBE_WAIT_INTERVAL {
                continue;
            }
        } else {
            // Core: masternode/utils.cpp:46-48.
            if !peer.mn_conn {
                continue;
            }
            if let Some(verified) = peer.verified.filter(|hash| *hash != [0; 32]) {
                // Core: masternode/utils.cpp:49-60.
                if is_quorum_node(registry, verified)
                    || registry.is_relay_member(&verified)
                    || peer.inbound
                {
                    continue;
                }
            } else {
                // Core: masternode/utils.cpp:62-68.
                if now - peer.connected_at < PROBE_WAIT_INTERVAL || peer.qwatch {
                    continue;
                }
            }
        }

        // Core: masternode/utils.cpp:71-74.
        if mixing_addrs.iter().any(|address| address == &peer.addr) {
            continue;
        }

        let line = if log_ips {
            format!("Closing Masternode connection: peer={}, addr={}", peer.id, peer.addr)
        } else {
            format!("Closing Masternode connection: peer={}", peer.id)
        };
        result.log.push(line);
        result.disconnect.push(peer.id);
    }
    result
}

fn is_verified(verified: Option<[u8; 32]>) -> bool {
    verified.is_some_and(|hash| hash != [0; 32])
}

fn is_quorum_node(registry: &QuorumNodes, verified: [u8; 32]) -> bool {
    registry.quorum_groups().iter().any(|nodes| nodes.contains(&verified))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use dash_params::LlmqType;

    use super::*;

    fn hash(byte: u8) -> [u8; 32] {
        [byte; 32]
    }

    fn addr(port: u16) -> Service {
        Service::new(dash_netaddr::NetAddr::ipv4([127, 0, 0, 1]), port)
    }

    fn peer(id: i64, verified: Option<[u8; 32]>) -> MaintenancePeer {
        MaintenancePeer {
            id,
            inbound: false,
            feeler: false,
            manual: false,
            mn_conn: true,
            probe: false,
            verified,
            connected_at: 0,
            qwatch: false,
            addr: addr(id as u16),
        }
    }

    fn registry_with_nodes(nodes: &[[u8; 32]]) -> QuorumNodes {
        let registry = QuorumNodes::default();
        registry.set_nodes(LlmqType::LlmqTest, hash(99), HashSet::from_iter(nodes.iter().copied()));
        registry
    }

    #[test]
    fn maintenance_count_gate_is_strictly_less_than_max_outbound() {
        let registry = QuorumNodes::default();
        let peers = [peer(1, None), peer(2, None)];
        assert!(
            do_maintenance(true, false, &peers[..1], 2, 10, &registry, &[], false)
                .disconnect
                .is_empty()
        );
        assert_eq!(
            do_maintenance(true, false, &peers, 2, 10, &registry, &[], false).disconnect,
            vec![1, 2]
        );
        assert_eq!(
            do_maintenance(true, false, &peers, 3, 10, &registry, &[], false),
            MaintenanceResult::default()
        );
    }

    #[test]
    fn maintenance_keeps_probe_and_unverified_connections_for_four_seconds() {
        let registry = QuorumNodes::default();
        let mut probe = peer(1, None);
        probe.probe = true;
        let unverified = peer(2, None);
        let peers = [probe, unverified];
        assert!(
            do_maintenance(true, false, &peers, 2, 4, &registry, &[], false).disconnect.is_empty()
        );
        assert_eq!(
            do_maintenance(true, false, &peers, 2, 5, &registry, &[], false).disconnect,
            vec![1, 2]
        );
    }

    #[test]
    fn maintenance_keeps_verified_quorum_relay_inbound_qwatch_mixing_and_normal_peers() {
        let verified_quorum = hash(1);
        let verified_relay = hash(2);
        let registry = registry_with_nodes(&[verified_quorum]);
        registry.set_nodes(LlmqType::LlmqTest, hash(98), HashSet::from([verified_relay]));
        registry.set_relay_members(
            LlmqType::LlmqTest,
            hash(97),
            HashSet::from([verified_relay]),
            &[],
        );
        let mut inbound = peer(3, Some(hash(3)));
        inbound.inbound = true;
        let mut qwatch = peer(4, None);
        qwatch.qwatch = true;
        let mut normal = peer(5, Some(hash(5)));
        normal.mn_conn = false;
        let mut mixing = peer(6, Some(hash(6)));
        mixing.addr = addr(20006);
        let peers = [
            peer(1, Some(verified_quorum)),
            peer(2, Some(verified_relay)),
            inbound,
            qwatch,
            normal,
            mixing.clone(),
            peer(7, Some(hash(7))),
        ];
        let result = do_maintenance(true, false, &peers, 1, 10, &registry, &[mixing.addr], false);
        assert_eq!(result.disconnect, vec![7]);
        assert_eq!(result.log, vec!["Closing Masternode connection: peer=7"]);
    }

    #[test]
    fn maintenance_uses_both_core_logips_forms() {
        let registry = QuorumNodes::default();
        let peer = peer(9, Some(hash(9)));
        assert_eq!(
            do_maintenance(true, false, std::slice::from_ref(&peer), 0, 10, &registry, &[], false,)
                .log,
            vec!["Closing Masternode connection: peer=9"]
        );
        assert_eq!(
            do_maintenance(true, false, &[peer], 0, 10, &registry, &[], true).log,
            vec![format!("Closing Masternode connection: peer=9, addr={}", addr(9))]
        );
    }
}
