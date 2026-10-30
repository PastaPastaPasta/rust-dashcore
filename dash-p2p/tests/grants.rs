//! A connection's slot (Core's `CSemaphoreGrant`) is released when the connection ends, even
//! while the processor still holds the peer's handle: Core's `grantOutbound` is a `CNode`
//! member that `DisconnectNodes` releases when it takes the node out of the table, and our handle
//! can outlive the table entry. A refused `attach` (the core interrupted meanwhile) releases it
//! too.
// Core: net.cpp:2166-2176 DisconnectNodes; net.cpp:3636 OpenNetworkConnection

mod common;

use common::{attach_with, core, run_until};
use dash_p2p::{DialLoop, Grant, MemSocket, NetCore, PeerHandle};

/// Every outbound slot, so the next `try_grant` succeeds only once one is released.
fn all_grants(core: &NetCore) -> Vec<Grant> {
    std::iter::from_fn(|| core.try_grant(DialLoop::OpenCon)).collect()
}

fn attach(core: &NetCore, sock: MemSocket, grant: Grant) -> PeerHandle {
    attach_with(core, sock, [10, 0, 0, 1], |a| a.grant = Some(grant))
}

#[tokio::test(start_paused = true)]
async fn a_removed_peer_releases_its_grant_while_its_handle_is_held() {
    let core = core();
    let mut held = all_grants(&core);
    let grant = held.pop().unwrap();
    let (ours, theirs) = MemSocket::pair();
    let peer = attach(&core, ours, grant);
    assert!(core.try_grant(DialLoop::OpenCon).is_none());

    theirs.close();
    run_until(|| core.peers().is_empty()).await;
    assert!(core.try_grant(DialLoop::OpenCon).is_some(), "the handle kept the slot");
    drop(peer);
}

#[tokio::test]
async fn a_refused_attach_releases_its_grant() {
    let core = core();
    let mut held = all_grants(&core);
    let grant = held.pop().unwrap();
    core.interrupt();
    let (ours, _theirs) = MemSocket::pair();
    let peer = attach(&core, ours, grant);
    assert!(core.peer(peer.id()).is_none());
    assert!(core.try_grant(DialLoop::OpenCon).is_some(), "the refused peer kept the slot");
}
