//! P8 S0b: `NetSlot` implementations of `add_peer_address`, `good_peer_address`, `lookup_host`,
//! `addrman_cjdns_reachable`, `node_addresses` and `add_connection` against a real regtest
//! `NetCore`, `AddrMan` and `ConnMan`.
// Core: v24.0.2 src/rpc/net.cpp:380-440, :939-998, :1000-1054; src/net.cpp:2040-2075, :4277-4286

use std::sync::Arc;

use dash_p2p::{
    Address, Attach, ConnectionType, MemSocket, NetAddr, Network, Permissions, Service,
    ServiceFlags, SubNet,
};
use dashd_rpc::peers::{AddConnectionType, AddrmanCounts, NodeAddress, NodeNet};

use super::amb::node;
use crate::rpc::NetSlot;

fn attach_outbound(slot: &NetSlot, i: u8, conn_type: ConnectionType, far: &mut Vec<MemSocket>) {
    let (ours, theirs) = MemSocket::pair();
    far.push(theirs);
    slot.core().expect("attached").attach(Attach {
        sock: Arc::new(ours),
        addr: Address {
            service: Service::new(NetAddr::ipv4([30 + i, 1, 1, 1]), 19899),
            services: ServiceFlags::NODE_NETWORK,
            time: 0,
        },
        addr_bind: Service::default(),
        dest: None,
        conn_type,
        inbound_onion: false,
        permissions: Permissions::NONE,
        use_v2: false,
        grant: None,
        masternode: false,
        masternode_probe: false,
    });
}

#[test]
fn net_slot_addrman_and_getnodeaddresses_and_addconnection() {
    let dir = tempfile::tempdir().unwrap();
    let node = node(dir.path(), &["-connect=0", "-dnsseed=0", "-fixedseeds=0", "-listen=0"]);
    let slot = node.rpc_net();
    let core = slot.core().expect("attached");
    let net: &dyn NodeNet = &*slot;

    // Detached slot returns None everywhere.
    let detached = NetSlot::default();
    let v4 = Service::new(NetAddr::ipv4([1, 2, 3, 4]), 9999);
    assert_eq!(NodeNet::addrman_cjdns_reachable(&detached), None);
    assert_eq!(NodeNet::add_peer_address(&detached, &v4, 1, 1_700_000_000, &v4.addr), None);
    assert_eq!(NodeNet::node_addresses(&detached, 0, None), None);
    assert_eq!(
        NodeNet::add_connection(&detached, "127.0.0.1:1", AddConnectionType::BlockRelay, false),
        None
    );

    // Without -cjdnsreachable, CJDNS is not reachable and fc00::1 stays IPv6 (RFC4193, unroutable),
    // so AddrMan::Add refuses it.
    let now = core.clock().node_now().0;
    assert_eq!(net.addrman_cjdns_reachable(), Some(false));
    let fc = net.lookup_host("fc00::1").expect("fc00::1 parses numerically");
    let fc = fc.maybe_flip_to_cjdns(false);
    assert_eq!(fc.net(), Network::Ipv6);
    assert_eq!(net.add_peer_address(&Service::new(fc.clone(), 9999), 1, now, &fc), Some(false));
    assert_eq!(core.addrman().size(None, None), 0);

    // Add an IPv4 address (new table only, not tried).
    assert_eq!(
        net.add_peer_address(&v4, ServiceFlags::NODE_NETWORK.get(), now, &v4.addr),
        Some(true)
    );
    assert_eq!(core.addrman().size(None, Some(true)), 1);
    assert_eq!(core.addrman().size(None, Some(false)), 0);
    let info1 = core.addrman().get_address_info(&v4).expect("v4 stored in addrman");
    assert_eq!(info1.service, v4);
    assert_eq!(info1.services, ServiceFlags::NODE_NETWORK);
    assert_eq!(info1.time, i64::from(now as u32));
    assert_eq!(info1.source, v4.addr);
    assert!(!info1.in_tried);

    // Scoped IPv6 through NetSlot::lookup_host (libc getaddrinfo(AI_NUMERICHOST)), then Good.
    let v6_addr = net.lookup_host("[2a01::1%1]").expect("scoped IPv6 resolves on node path");
    #[cfg(unix)]
    assert_eq!(v6_addr.scope_id(), 1);
    let v6 = Service::new(v6_addr, 8333);
    assert_eq!(
        net.add_peer_address(&v6, ServiceFlags::NODE_NETWORK.get(), now, &v6.addr),
        Some(true)
    );
    net.good_peer_address(&v6);
    assert_eq!(core.addrman().size(None, Some(false)), 1);
    let info2 = core.addrman().get_address_info(&v6).expect("v6 stored in addrman");
    assert_eq!(info2.service, v6);
    assert_eq!(info2.time, i64::from(now as u32));
    assert!(info2.in_tried);

    // NetSlot::node_addresses: count, network filter, and banned/discouraged filtering.
    let all = net.node_addresses(0, None).expect("attached");
    assert_eq!(all.len(), 2);
    assert_eq!(net.node_addresses(1, None).expect("attached").len(), 1);
    assert_eq!(
        net.node_addresses(0, Some(Network::Ipv4)).expect("attached"),
        vec![NodeAddress {
            time: i64::from(now as u32),
            services: ServiceFlags::NODE_NETWORK.get(),
            address: "1.2.3.4".into(),
            port: 9999,
            network: "ipv4",
        }]
    );
    let only_v6 = net.node_addresses(0, Some(Network::Ipv6)).expect("attached");
    assert_eq!(only_v6.len(), 1);
    assert_eq!(only_v6[0].network, "ipv6");
    assert_eq!(only_v6[0].port, 8333);
    #[cfg(unix)]
    assert_eq!(only_v6[0].address, "2a01::1%1");
    assert!(net.node_addresses(0, Some(Network::Onion)).expect("attached").is_empty());

    core.ban(SubNet::single(v4.addr.clone()), 3600, false);
    core.banlist().discourage(&v6.addr);
    assert!(net.node_addresses(0, None).expect("attached").is_empty());

    // NetSlot::add_connection maps each AddConnectionType to the right ConnectionType cap. The
    // network stays active: an inactive network's sweep drops the attached cap peers after LINGER.
    // A granted open dials 127.0.0.1:1, which is refused at once and releases its grant.
    let cfg = core.config();
    let max_br = cfg.max_outbound_block_relay;
    let max_fr = cfg.max_outbound_full_relay;
    assert!(max_br > 0 && max_fr > 0);
    let mut far = Vec::new();
    for i in 0..max_br {
        attach_outbound(&slot, i as u8, ConnectionType::BlockRelay, &mut far);
    }
    assert_eq!(
        net.add_connection("127.0.0.1:1", AddConnectionType::BlockRelay, false),
        Some(false),
        "BlockRelay hits max_outbound_block_relay"
    );
    assert_eq!(
        net.add_connection("127.0.0.1:1", AddConnectionType::OutboundFullRelay, false),
        Some(true),
        "OutboundFullRelay does not hit BlockRelay cap"
    );

    for i in 0..max_fr {
        attach_outbound(&slot, (max_br + i) as u8, ConnectionType::OutboundFullRelay, &mut far);
    }
    assert_eq!(
        net.add_connection("127.0.0.1:1", AddConnectionType::OutboundFullRelay, false),
        Some(false),
        "OutboundFullRelay hits max_outbound_full_relay"
    );
    assert_eq!(
        net.add_connection("127.0.0.1:1", AddConnectionType::AddrFetch, false),
        Some(true),
        "AddrFetch has no per-type cap"
    );
    assert_eq!(
        net.add_connection("127.0.0.1:1", AddConnectionType::Feeler, false),
        Some(true),
        "Feeler has no per-type cap"
    );
}

/// BL-690a: `NetSlot` implementations of `send_msg_to_peer`, `addrman_info`, `raw_addrman`,
/// `lookup_service`, and `open_masternode_connection`.
// Core: v24.0.2 src/rpc/net.cpp:1056-1244; src/rpc/masternode.cpp:47-87
#[test]
fn net_slot_bl690a_sendmsg_addrman_and_masternode_connect() {
    let dir = tempfile::tempdir().unwrap();
    let node = node(dir.path(), &["-connect=0", "-dnsseed=0", "-fixedseeds=0", "-listen=0"]);
    let slot = node.rpc_net();
    let core = slot.core().expect("attached");
    let net: &dyn NodeNet = &*slot;

    // Detached slot returns None everywhere.
    let detached = NetSlot::default();
    let v4 = Service::new(NetAddr::ipv4([1, 2, 3, 4]), 8333);
    assert_eq!(NodeNet::send_msg_to_peer(&detached, 0, "addr", vec![]), None);
    assert_eq!(NodeNet::addrman_info(&detached), None);
    assert_eq!(NodeNet::raw_addrman(&detached), None);
    assert_eq!(NodeNet::open_masternode_connection(&detached, &v4, false), None);

    // lookup_service: numeric only, port defaults to 0 when omitted.
    assert_eq!(
        net.lookup_service("127.0.0.1"),
        Some(Service::new(NetAddr::ipv4([127, 0, 0, 1]), 0))
    );
    assert_eq!(
        net.lookup_service("127.0.0.1:19999"),
        Some(Service::new(NetAddr::ipv4([127, 0, 0, 1]), 19999))
    );
    assert_eq!(net.lookup_service("localhost:19999"), None);

    // send_msg_to_peer: requires FullyConnectedOnly (not-yet-connected peer returns Some(false)).
    let (ours, _theirs) = MemSocket::pair();
    let peer = core.attach(Attach {
        sock: Arc::new(ours),
        addr: Address {
            service: Service::new(NetAddr::ipv4([40, 1, 1, 1]), 19899),
            services: ServiceFlags::NODE_NETWORK,
            time: 0,
        },
        addr_bind: Service::default(),
        dest: None,
        conn_type: ConnectionType::Manual,
        inbound_onion: false,
        permissions: Permissions::NONE,
        use_v2: false,
        grant: None,
        masternode: false,
        masternode_probe: false,
    });
    assert_eq!(net.send_msg_to_peer(peer.id(), "addr", vec![0xff]), Some(false));
    peer.set_successfully_connected_for_tests();
    assert_eq!(net.send_msg_to_peer(peer.id(), "addr", vec![0xff]), Some(true));
    assert_eq!(net.send_msg_to_peer(peer.id() + 999, "addr", vec![0xff]), Some(false));

    // addrman_info & raw_addrman reflect added new/tried entries.
    let now = core.clock().node_now().0;
    let tried_svc = Service::new(NetAddr::ipv4([1, 2, 3, 4]), 8333);
    let new_svc = Service::new(NetAddr::ipv4([2, 0, 0, 0]), 8333);
    assert_eq!(
        net.add_peer_address(&tried_svc, ServiceFlags::NODE_NETWORK.get(), now, &tried_svc.addr),
        Some(true)
    );
    net.good_peer_address(&tried_svc);
    assert_eq!(
        net.add_peer_address(&new_svc, ServiceFlags::NODE_NETWORK.get(), now, &new_svc.addr),
        Some(true)
    );

    let info = net.addrman_info().expect("attached");
    assert_eq!(
        info.ipv4,
        AddrmanCounts {
            new: 1,
            tried: 1,
            total: 2,
        }
    );
    assert_eq!(info.all_networks, info.ipv4);
    assert_eq!(info.ipv6, AddrmanCounts::default());

    let raw = net.raw_addrman().expect("attached");
    assert_eq!(raw.new.len(), 1);
    assert_eq!(raw.tried.len(), 1);
    assert_eq!(raw.new[0].address, "2.0.0.0");
    assert_eq!(raw.new[0].port, 8333);
    assert_eq!(raw.new[0].services, ServiceFlags::NODE_NETWORK.get());
    assert_eq!(raw.new[0].network, "ipv4");
    assert_eq!(raw.new[0].source, "2.0.0.0");
    assert_eq!(raw.new[0].source_network, "ipv4");
    assert_eq!(raw.tried[0].address, "1.2.3.4");
    assert_eq!(raw.tried[0].network, "ipv4");

    // open_masternode_connection: succeeds to a local TcpListener and sets masternode flags,
    // fails to a closed port (port 0).
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let listen_port = listener.local_addr().unwrap().port();
    let accept_thread = std::thread::spawn(move || listener.accept().ok());
    let target = Service::new(NetAddr::ipv4([127, 0, 0, 1]), listen_port);
    assert_eq!(net.open_masternode_connection(&target, false), Some(true));
    let mn_peer = core
        .peers()
        .iter()
        .find(|p| p.addr().service == target)
        .cloned()
        .expect("masternode peer attached");
    assert!(mn_peer.masternode_connection());
    assert!(mn_peer.masternode_probe());
    assert_eq!(mn_peer.conn_type(), ConnectionType::OutboundFullRelay);
    // Calling again when already connected still reports Some(true) via IsConnected(..., AllNodes).
    assert_eq!(net.open_masternode_connection(&target, false), Some(true));

    let _accepted = accept_thread.join().unwrap();
    let port_zero = Service::new(NetAddr::ipv4([127, 0, 0, 1]), 0);
    assert_eq!(net.open_masternode_connection(&port_zero, false), Some(false));
}
