//! The active masternode's reads of the local-host table and its inbound check.
// Core: active/masternode.cpp:17-48, :159-161 at v24.0.0

mod common;

use common::{core, core_with};
use dash_p2p::local::LocalScore;
use dash_p2p::{NetAddr, Network, Service};

fn google() -> NetAddr {
    NetAddr::ipv4([8, 8, 8, 8])
}

// Core: active/masternode.cpp:17-48 at v24.0.0
#[tokio::test]
async fn core_local_toward_an_ipv4_partner_prefers_ipv4_and_skips_onion() {
    let core = core();
    assert_eq!(core.local_toward(&google()), None);
    let onion =
        NetAddr::set_special("pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscryd.onion")
            .unwrap();
    core.set_reachable(Network::Onion, true);
    assert!(core.add_local(Service::new(onion, 9999), LocalScore::Manual));
    assert_eq!(core.local_toward(&google()), None, "an onion address is never toward IPv4");
    let v6 = NetAddr::from_ip("2001:db9::1".parse().unwrap());
    assert!(core.add_local(Service::new(v6.clone(), 9998), LocalScore::Manual));
    assert_eq!(core.local_toward(&google()), Some(Service::new(v6, 9998)));
    let v4 = NetAddr::ipv4([1, 2, 3, 4]);
    assert!(core.add_local(Service::new(v4.clone(), 9997), LocalScore::If));
    assert_eq!(core.local_toward(&google()), Some(Service::new(v4, 9997)));
}

// Core: active/masternode.cpp:19-20 at v24.0.0
#[tokio::test]
async fn core_local_toward_needs_listen() {
    let core = core_with(Default::default(), |cfg| cfg.listen = false);
    core.add_local(Service::new(NetAddr::ipv4([1, 2, 3, 4]), 9999), LocalScore::Manual);
    assert_eq!(core.local_toward(&google()), None);
}

// Core: active/masternode.cpp:159-161; util/sock.cpp:37-44 at v24.0.0
#[tokio::test]
async fn core_inbound_check_connects_once_and_closes() {
    let core = core();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let ours = Service::new(NetAddr::ipv4([127, 0, 0, 1]), port);
    assert!(core.connect_directly_selectable(&ours).await);
    drop(listener);
    assert!(!core.connect_directly_selectable(&ours).await);
}
