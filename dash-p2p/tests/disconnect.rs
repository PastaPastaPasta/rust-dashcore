//! The disconnect path's timing and the handle `discourage` takes (SB6 Opus-high M1 and m5).
//!
//! Core's handler keeps a reference on the node, so `MaybeDiscourageAndDisconnect` discourages
//! a peer the socket thread already removed (net.cpp:3696-3712, net_processing.cpp:5833-5870).
//! Core's socket thread runs `DisconnectNodes` once per `SocketHandler` pass: a mark waits for
//! the next pass (500 ms, or a socket event: any peer's read or send, an accept or a new peer), the linger sends `SHUT_WR`, and the remote's close
//! after it ends the linger early (net.cpp:111-120, 2486-2490, 2620-2640). A lingering socket
//! that turns readable keeps Core's loop polling, so our side is shut at once
//! (net.cpp:2376-2415).
//!
//! The tests run on a paused current-thread runtime: yielding lets every task run without
//! moving time, so "before the next pass" is exact.

mod common;

#[cfg(feature = "inbound")]
use std::sync::Arc;

use common::{PASS, attach, attach_with, core, core_with, run_until, settle, settle_until};
use dash_p2p::conn::{ConnectionType, Permissions, Why};
use dash_p2p::sock::NetSocket;
#[cfg(feature = "inbound")]
use dash_p2p::{Address, NetAddr, Service, ServiceFlags};
use dash_p2p::{CommandString, MemSocket, OutMsg};

/// The peer's remote end reads EOF: our side sent `SHUT_WR` (or closed).
fn remote_sees_eof(theirs: &MemSocket) -> bool {
    matches!(theirs.try_read(&mut [0u8; 1]), Ok(0))
}

/// M1: a peer that misbehaved and then closed is gone from the table when the processor
/// discourages it; the handle still reaches the address.
#[tokio::test(start_paused = true)]
async fn discourage_reaches_a_peer_the_sweep_removed() {
    let core = core();
    let (ours, theirs) = MemSocket::pair();
    let peer = attach(&core, ours, [10, 0, 0, 1]);
    let addr = peer.addr().service.addr.clone();
    theirs.close();
    run_until(|| core.peers().is_empty()).await;
    assert!(core.peer(peer.id()).is_none());

    assert!(core.discourage(&peer));
    assert!(core.banlist().is_discouraged(&addr));

    // A second connection from the same address is marked too (`DisconnectNode(addr)`).
    let (ours2, _theirs2) = MemSocket::pair();
    let again = attach(&core, ours2, [10, 0, 0, 1]);
    let (ours3, _theirs3) = MemSocket::pair();
    let other = attach(&core, ours3, [10, 0, 0, 2]);
    assert!(core.discourage(&peer));
    assert!(again.disconnect_requested());
    assert!(!other.disconnect_requested());
}

/// M1: Core's exemptions hold on a removed handle too, in Core's order: `noban`, then manual,
/// then a local address (disconnected but not discouraged). A `noban` or manual peer on a local
/// address is kept, not disconnected.
// Core: net_processing.cpp:5845-5862
#[tokio::test(start_paused = true)]
async fn discourage_exemptions_hold_after_removal() {
    let core = core();
    let cases = [
        ([10, 0, 0, 3], ConnectionType::Inbound, Permissions::NO_BAN, false),
        ([10, 0, 0, 4], ConnectionType::Manual, Permissions::NONE, false),
        ([127, 0, 0, 1], ConnectionType::Inbound, Permissions::NONE, true),
        ([127, 0, 0, 1], ConnectionType::Inbound, Permissions::NO_BAN, false),
        ([127, 0, 0, 1], ConnectionType::Manual, Permissions::NONE, false),
    ];
    for (ip, conn_type, permissions, disconnected) in cases {
        let (ours, theirs) = MemSocket::pair();
        let peer = attach_with(&core, ours, ip, |a| {
            a.conn_type = conn_type;
            a.permissions = permissions;
        });
        theirs.close();
        run_until(|| core.peer(peer.id()).is_none()).await;
        assert_eq!(core.discourage(&peer), disconnected, "{conn_type:?} {permissions:?} {ip:?}");
        assert!(!core.banlist().is_discouraged(&peer.addr().service.addr), "{ip:?}");
    }
}

/// m5: a mark does not wake the sweep; the linger starts on the next 500 ms pass.
#[tokio::test(start_paused = true)]
async fn a_mark_waits_for_the_next_pass() {
    let core = core();
    let (ours, _theirs) = MemSocket::pair();
    let peer = attach(&core, ours, [10, 0, 0, 1]);
    settle().await;

    core.disconnect(peer.id(), Why::Processor("test"));
    settle().await;
    assert!(peer.disconnect_requested());
    assert!(peer.linger_until().is_none(), "a mark woke the sweep");

    tokio::time::sleep(PASS).await;
    settle().await;
    assert!(peer.linger_until().is_some());
}

/// R-m1: another peer's read is a socket event, so the pass it runs starts a marked peer's
/// linger (and sends its FIN) at once, as Core's wait returns on any socket's readiness
/// (net.cpp:2423). Four bytes, a partial header, complete nothing and close nothing.
#[tokio::test(start_paused = true)]
async fn another_peers_read_starts_the_linger() {
    let core = core();
    let (a_ours, a_theirs) = MemSocket::pair();
    let (b_ours, b_theirs) = MemSocket::pair();
    let a = attach(&core, a_ours, [10, 0, 0, 1]);
    let b = attach(&core, b_ours, [10, 0, 0, 2]);
    settle().await;
    core.disconnect(a.id(), Why::Processor("test"));
    settle().await;
    assert!(a.linger_until().is_none());

    b_theirs.inject(&[0; 4]);
    assert!(settle_until(|| a.linger_until().is_some()).await, "the read ran no pass");
    assert!(remote_sees_eof(&a_theirs), "the linger sent no FIN");
    assert!(!b.disconnect_requested());
}

/// R-m1: a push to another peer's empty queue is sent within a pass (Core's wakeup pipe,
/// net.cpp:4896-4904), which starts a marked peer's linger at once.
#[tokio::test(start_paused = true)]
async fn a_push_to_another_peer_starts_the_linger() {
    let core = core();
    let (a_ours, a_theirs) = MemSocket::pair();
    let (b_ours, b_theirs) = MemSocket::pair();
    let a = attach(&core, a_ours, [10, 0, 0, 1]);
    let b = attach(&core, b_ours, [10, 0, 0, 2]);
    settle().await;
    core.disconnect(a.id(), Why::Processor("test"));
    settle().await;
    assert!(a.linger_until().is_none());

    core.push(&b, OutMsg::new(CommandString::from_static("ping"), vec![0; 8]));
    assert!(settle_until(|| a.linger_until().is_some()).await, "the send ran no pass");
    assert!(remote_sees_eof(&a_theirs), "the linger sent no FIN");
    assert!(!b_theirs.drain().is_empty());
}

/// R-m1: Core writes its wakeup pipe once a new node is listed (net.cpp:2021-2022, 3663-3664),
/// so attaching another peer runs a pass that starts a marked peer's linger.
#[tokio::test(start_paused = true)]
async fn attaching_another_peer_starts_the_linger() {
    let core = core();
    let (a_ours, _a_theirs) = MemSocket::pair();
    let a = attach(&core, a_ours, [10, 0, 0, 1]);
    settle().await;
    core.disconnect(a.id(), Why::Processor("test"));
    settle().await;
    assert!(a.linger_until().is_none());

    let (b_ours, _b_theirs) = MemSocket::pair();
    attach(&core, b_ours, [10, 0, 0, 2]);
    assert!(settle_until(|| a.linger_until().is_some()).await, "the attach ran no pass");
}

/// R-m1: a listening socket's readiness ends Core's wait (net.cpp:2423, 2579-2590), so even a
/// refused `accept` runs a pass that starts a marked peer's linger.
#[cfg(feature = "inbound")]
#[tokio::test(start_paused = true)]
async fn a_refused_accept_starts_the_linger() {
    let core = core();
    let (a_ours, _a_theirs) = MemSocket::pair();
    let a = attach(&core, a_ours, [10, 0, 0, 1]);
    settle().await;
    core.disconnect(a.id(), Why::Processor("test"));
    settle().await;
    assert!(a.linger_until().is_none());

    let (b_ours, _b_theirs) = MemSocket::pair();
    let addr = Address {
        service: Service::new(NetAddr::ipv4([10, 0, 0, 2]), 19899),
        services: ServiceFlags::NODE_NETWORK,
        time: 0,
    };
    let refused =
        core.accept(Arc::new(b_ours), addr, Service::default(), Permissions::NONE, false, true);
    assert!(refused.is_none());
    assert!(settle_until(|| a.linger_until().is_some()).await, "the accept ran no pass");
}

/// m5: the linger's `SHUT_WR` reaches the remote before the linger ends, and the remote's close
/// after it ends the linger at once, not on the next pass.
#[tokio::test(start_paused = true)]
async fn the_remotes_close_after_our_fin_ends_the_linger() {
    let core = core();
    let (ours, theirs) = MemSocket::pair();
    let peer = attach(&core, ours, [10, 0, 0, 1]);
    settle().await;
    core.disconnect(peer.id(), Why::Processor("test"));
    run_until(|| peer.linger_until().is_some()).await;
    settle().await;

    assert!(remote_sees_eof(&theirs), "the linger sent no FIN");
    assert!(core.peer(peer.id()).is_some());

    let start = tokio::time::Instant::now();
    theirs.close();
    assert!(settle_until(|| core.peers().is_empty()).await, "the close waited for a pass");
    assert!(start.elapsed() < PASS);
}

/// Their close of a live, flushed peer is Core's graceful close: the reader's EOF sets
/// `fOtherSideDisconnected` ("avoid lingering", net.cpp:2619-2627), so the next pass, which the
/// read runs at once, removes the peer without a linger (P2a-net D33, Opus wire r1 O1).
#[tokio::test(start_paused = true)]
async fn their_close_of_a_live_peer_never_lingers() {
    let core = core();
    let (ours, theirs) = MemSocket::pair();
    let peer = attach(&core, ours, [10, 0, 0, 1]);
    settle().await;
    assert!(peer.send_queue_empty() && !peer.disconnect_requested());

    let start = tokio::time::Instant::now();
    theirs.close();
    assert!(settle_until(|| core.peers().is_empty()).await, "their close waited for a pass");
    assert!(peer.other_side_disconnected());
    assert_eq!(peer.linger_until(), None, "their close lingered");
    assert!(start.elapsed() < PASS);
}

/// D43: their FIN before our `SHUT_WR` makes the lingering socket readable, so our side is shut
/// at once (no 500 ms wait), and their FIN then ends the linger.
#[tokio::test(start_paused = true)]
async fn their_fin_before_ours_shuts_our_side_at_once() {
    let core = core();
    let (ours, theirs) = MemSocket::pair();
    let peer = attach(&core, ours, [10, 0, 0, 1]);
    settle().await;
    core.disconnect(peer.id(), Why::Processor("test"));
    settle().await;
    assert!(peer.linger_until().is_none());

    let start = tokio::time::Instant::now();
    theirs.shutdown_write().unwrap();
    assert!(settle_until(|| core.peers().is_empty()).await, "their FIN waited for a pass");
    assert!(remote_sees_eof(&theirs));
    assert!(start.elapsed() < PASS);
}

/// D43: their FIN alone is no hang-up. While our queue cannot flush, no `SHUT_WR` is sent and
/// the peer lingers on to its deadline.
#[tokio::test(start_paused = true)]
async fn their_fin_alone_does_not_end_the_linger() {
    let core = core();
    let (ours, theirs) = MemSocket::pair();
    ours.set_send_budget(Some(0));
    let peer = attach(&core, ours, [10, 0, 0, 1]);
    core.push(&peer, OutMsg::new(CommandString::from_static("ping"), vec![0; 8]));
    settle().await;
    core.disconnect(peer.id(), Why::Processor("test"));
    settle().await;

    theirs.shutdown_write().unwrap();
    settle().await;
    assert!(peer.linger_until().is_some(), "their FIN did not wake the sweep");
    assert!(core.peer(peer.id()).is_some(), "their FIN alone ended the linger");
    assert!(!remote_sees_eof(&theirs));
    run_until(|| core.peers().is_empty()).await;
}

/// D43: bytes reaching a lingering peer make its socket readable: our side is shut at once,
/// and the bytes are left to the close at the linger's end.
#[tokio::test(start_paused = true)]
async fn unread_bytes_shut_our_side_at_once() {
    let core = core();
    let (ours, theirs) = MemSocket::pair();
    let peer = attach(&core, ours, [10, 0, 0, 1]);
    settle().await;
    core.disconnect(peer.id(), Why::Processor("test"));
    settle().await;

    theirs.inject(&[0; 24]);
    assert!(settle_until(|| remote_sees_eof(&theirs)).await, "no FIN before the next pass");
    assert!(core.peer(peer.id()).is_some());
    assert_eq!(theirs.in_flight(), 24);
    run_until(|| core.peers().is_empty()).await;
}

/// m11: an outbound V2 connection that closes before the peer sent a byte queues a V1 retry
/// (Core's `m_reconnections`), unless the host turned `v1_retry` off (SPV, D45).
#[tokio::test(start_paused = true)]
async fn a_v2_close_before_any_byte_queues_a_v1_retry_unless_off() {
    for on in [true, false] {
        let core = core_with(Default::default(), |cfg| cfg.v1_retry = on);
        let (ours, theirs) = MemSocket::pair();
        attach_with(&core, ours, [10, 0, 0, 1], |a| a.use_v2 = true);
        run_until(|| !theirs.drain().is_empty()).await;
        theirs.close();
        run_until(|| core.peers().is_empty()).await;
        let retry = core.take_reconnection();
        assert_eq!(retry.is_some(), on);
        if let Some(r) = retry {
            assert!(!r.use_v2);
        }
    }
}
