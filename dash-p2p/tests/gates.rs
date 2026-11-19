//! `gates::admit` stamps the first message after `verack` before the blocks-only gate, as Core's
//! `ProcessMessage` prologue does (v24.0.0-rc.3 `net_processing.cpp:4368-4385`): a first message
//! that violates blocks-only still records `nTimeFirstMessageReceived`, and on a masternode
//! probe it is the probe gate that disconnects (SB6 r5 m12).

use std::cell::Cell;

use dash_p2p::gates::{self, Gate, GatePeer};
use dash_p2p::policy;
use dash_p2p::{CommandString, MemSocket, PeerHandle, PeerId};

mod common;

const NOW: i64 = 1_700_000_000;

/// A peer after `version` and `verack`, with no message since.
#[derive(Default)]
struct Peer {
    first_time: Cell<i64>,
    first_mnauth: Cell<bool>,
    probe: bool,
    disconnect: Cell<bool>,
}

impl GatePeer for Peer {
    fn id(&self) -> PeerId {
        7
    }
    fn version(&self) -> i32 {
        70242
    }
    fn successfully_connected(&self) -> bool {
        true
    }
    fn first_message_time(&self) -> i64 {
        self.first_time.get()
    }
    fn set_first_message(&self, time: i64, is_mnauth: bool) {
        self.first_time.set(time);
        self.first_mnauth.set(is_mnauth);
    }
    fn masternode_probe(&self) -> bool {
        self.probe
    }
    fn mark_disconnect(&self) {
        self.disconnect.set(true);
    }
}

fn blocks_only_commands() -> impl Iterator<Item = CommandString> {
    policy::table().iter().filter(|r| r.blocks_only_violation).map(|r| r.cmd)
}

#[test]
fn a_blocks_only_first_message_is_stamped_before_the_disconnect() {
    let mut n = 0;
    for cmd in blocks_only_commands() {
        let peer = Peer::default();
        assert_eq!(gates::admit(&peer, &cmd, NOW, true), Gate::BlocksOnlyViolation, "{cmd}");
        assert_eq!(peer.first_time.get(), NOW, "{cmd}: the stamp comes first");
        assert!(!peer.first_mnauth.get() && peer.disconnect.get(), "{cmd}");
        n += 1;
    }
    assert_eq!(n, 26);
}

#[test]
fn a_probe_whose_first_message_violates_blocks_only_fails_the_probe_gate() {
    for cmd in blocks_only_commands() {
        let peer = Peer {
            probe: true,
            ..Peer::default()
        };
        assert_eq!(gates::admit(&peer, &cmd, NOW, true), Gate::ProbeWithoutMnauth, "{cmd}");
        assert_eq!(peer.first_time.get(), NOW, "{cmd}");
        assert!(peer.disconnect.get(), "{cmd}");
    }
    // `mnauth` passes the probe gate and is not in the blocks-only set.
    let peer = Peer {
        probe: true,
        ..Peer::default()
    };
    let mnauth = CommandString::from_static("mnauth");
    assert_eq!(gates::admit(&peer, &mnauth, NOW, true), Gate::Admit);
    assert!(peer.first_mnauth.get() && !peer.disconnect.get());
}

/// A connected [`PeerHandle`] read through its own [`GatePeer`] impl, with `nVersion` set: the
/// network core sets it only in a `version` exchange this file has no harness for.
struct Connected(PeerHandle);

impl GatePeer for Connected {
    fn id(&self) -> PeerId {
        GatePeer::id(&self.0)
    }
    fn version(&self) -> i32 {
        70242
    }
    fn successfully_connected(&self) -> bool {
        GatePeer::successfully_connected(&self.0)
    }
    fn first_message_time(&self) -> i64 {
        GatePeer::first_message_time(&self.0)
    }
    fn set_first_message(&self, time: i64, is_mnauth: bool) {
        GatePeer::set_first_message(&self.0, time, is_mnauth);
    }
    fn masternode_probe(&self) -> bool {
        GatePeer::masternode_probe(&self.0)
    }
    fn mark_disconnect(&self) {
        GatePeer::mark_disconnect(&self.0);
    }
}

// Core: net_processing.cpp:4386-4396; evo/mnauth.cpp:132, :135 at v24.0.0
#[tokio::test]
async fn a_probe_flip_is_seen_by_the_probe_gate() {
    let core = common::core();
    let ping = CommandString::from_static("ping");
    for flip in [false, true] {
        let (socket, _remote) = MemSocket::pair();
        let peer = common::attach(
            &core,
            socket,
            [
                3,
                3,
                3,
                if flip {
                    4
                } else {
                    3
                },
            ],
        );
        peer.set_successfully_connected_for_tests();
        assert!(!peer.masternode_probe());
        if flip {
            peer.set_masternode_probe(true);
        }
        let want = if flip {
            Gate::ProbeWithoutMnauth
        } else {
            Gate::Admit
        };
        let gate_peer = Connected(peer.clone());
        assert_eq!(gates::admit(&gate_peer, &ping, NOW, false), want, "flip={flip}");
        assert_eq!(peer.first_message_time(), NOW, "flip={flip}");
        assert_eq!(peer.disconnect_requested(), flip, "flip={flip}");
    }
}
