//! `Sim::remove_disconnected` on a starved box (P2a-net D35): a sweep that starts a peer's
//! linger only after the removal's last clock step still sees it expire. Fuzz crash
//! 54c54dd34a18 was this, at Nice 19 under load: the removal stopped moving the clock after
//! nine steps and waited for a linger that could not end, until `PATIENCE`.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use dash_p2p::conn::Permissions;
use dash_p2p::{ConnectionType, Why};
use dash_params::{ChainParamsBuilder, NetworkKind, Release};
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::sim::Sim;

/// Longer than the removal's nine 200 ms clock steps.
const STARVED: Duration = Duration::from_secs(3);

#[test]
fn a_linger_the_sweep_starts_late_still_expires() {
    let params =
        Arc::new(ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0).build().unwrap());
    let identity = local_identity(&params, &IdentityOptions::new()).unwrap();
    let mut sim = Sim::new(params, identity);
    let peer = sim.connect(1, ConnectionType::OutboundFullRelay, Permissions::NONE);

    // Starve the core: both runtime workers block, so the sweep cannot run until the removal
    // has made all its steps.
    let busy = Arc::new(AtomicUsize::new(0));
    for _ in 0..2 {
        let busy = busy.clone();
        sim.rt.spawn(async move {
            busy.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(STARVED);
        });
    }
    while busy.load(Ordering::SeqCst) < 2 {
        std::thread::sleep(Duration::from_millis(1));
    }
    sim.core.disconnect(peer.handle.id(), Why::Processor("test"));
    assert_eq!(peer.handle.linger_until(), None, "the sweep lingered before the starvation");

    let steady = dash_clock::Clock::steady(&*sim.clock);
    sim.remove_disconnected();
    assert!(sim.core.peers().is_empty());
    assert!(peer.handle.linger_until().is_some(), "the peer never lingered");
    assert_eq!(dash_clock::Clock::steady(&*sim.clock), steady, "the steady clock was not put back");
}
