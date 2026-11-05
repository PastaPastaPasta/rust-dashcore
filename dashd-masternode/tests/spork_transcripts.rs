//! Spork transcript equality with dashd v24.0.0-rc.3 (SP0): each fixture under
//! `tests/fixtures/dashd-v24/` (`contrib/node/sp0-transcripts/capture.py`; provenance in the
//! directory's `PROVENANCE.md`) replayed by [`dashd_p2p::sim::transcript`] against a node whose
//! registry holds SP0's [`SporkHandler`].
//!
//! dashd's `inv`s of sporks (`RelayInv`) are compared too: P5a's tracker and relay complete the
//! handler (P5a D13). The sporks the node signs (`sporkupdate`) are compared byte for byte, so
//! the signatures are Core's (RFC 6979 nonces).

use std::sync::{Arc, OnceLock};

use dash_clock::{AdjustedTime, Clock, NodeSeconds};
use dash_evo::spork::{SporkId, id_by_name, spork_hash};
use dash_num::Hash256;
use dash_p2p::ConnectionType;
use dash_p2p::conn::Permissions;
use dash_p2p_core::{InvType, Inventory, Spork};
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;
use dashd_masternode::sporks::{SporkHandler, SporkManager};
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::logic::ProcessorOptions;
use dashd_p2p::sim::transcript::{Hooks, replay};
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, Sim, VirtualClock};
use dashd_p2p::{Registry, Seams};

mod common;

/// The sim's clock as the adjusted time, with no peer offsets; the sim makes the clock after the
/// registry, so it is set once the sim exists.
#[derive(Default)]
struct SimTime(OnceLock<Arc<VirtualClock>>);

impl AdjustedTime for SimTime {
    fn now(&self) -> NodeSeconds {
        self.0.get().expect("the sim's clock").node_now()
    }
}

struct SporkHooks(Arc<SporkManager>);

impl Hooks for SporkHooks {
    fn input(&mut self, sim: &mut Sim, f: &[&str]) -> bool {
        if f[0] != "sporkupdate" {
            return false;
        }
        let id = id_by_name(f[1]).expect("a spork name");
        let hash =
            self.0.update_spork(id, f[2].parse().unwrap()).expect("the node is the spork signer");
        // The RPC relays what it signed (rpc/misc.cpp `sporkupdate`).
        sim.shared.relay_inv(&Inventory {
            inv_type: InvType::Spork,
            hash: Hash256::from_lendian(hash),
        });
        true
    }
}

/// A regtest node whose registry holds SP0's handler; with the spork key if `signer`.
fn node(signer: bool) -> (Sim, Arc<SporkManager>) {
    let p = Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let id = local_identity(&p, &IdentityOptions::new()).unwrap();
    let time = Arc::new(SimTime::default());
    let sporks = Arc::new(SporkManager::new(&p, Arc::clone(&time) as _));
    if signer {
        sporks.set_priv_key(common::REGTEST_WIF).unwrap();
    }
    let mut registry = Registry::new();
    registry.register(Box::new(SporkHandler::new(Arc::clone(&sporks))));
    let sim = Sim::with(p, id, ProcessorOptions::default(), registry, Seams::default(), |cfg| {
        // dashd's default `-peertimeout` (`sim` departs from it).
        cfg.peer_timeout = 60;
    });
    assert!(time.0.set(Arc::clone(&sim.clock)).is_ok(), "one clock");
    (sim, sporks)
}

fn run(name: &str, signer: bool) {
    let path = format!("{}/tests/fixtures/dashd-v24/{name}.txt", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let (sim, sporks) = node(signer);
    replay(name, &text, sim, &mut SporkHooks(sporks));
}

#[test]
fn sporks_serve() {
    run("sporks-serve", true);
}

#[test]
fn sporks_receive() {
    run("sporks-receive", false);
}

/// The `inv`s `peer` was sent since the last call, once at least `n` frames have arrived.
fn invs(sim: &Sim, peer: &dashd_p2p::sim::SimPeer, n: usize) -> Vec<Inventory> {
    sim.sent_at_least(peer, n)
        .into_iter()
        .filter(|(cmd, _)| cmd.as_str() == "inv")
        .flat_map(|(_, p)| dash_p2p_core::Inv::decode(&mut &p[..]).unwrap().inventory)
        .collect()
}

/// Core's `spork` branch: `ReceivedResponse` for every decoded spork, valid or not; `ForgetTxHash`
/// and `RelayInv` only for one that changed the store. The relay reaches every peer, the source
/// and a block-relay-only peer included (P5a D11), once per peer (the known filter).
// Core: net_processing.cpp:5700-5722, 2546-2553, 6719-6747
#[test]
fn a_new_spork_is_relayed_to_every_peer_once() {
    // The spork from the signer: byte-equal to dashd's (`spork_signatures`).
    let (_, signer) = node(true);
    let hash = signer.update_spork(SporkId::SPORK_19_CHAINLOCKS_ENABLED, 0).unwrap();
    let good = signer.spork_by_hash(&hash).unwrap();
    let mut bad = good.clone();
    bad.sig[10] ^= 1;
    let g = Inventory {
        inv_type: InvType::Spork,
        hash: Hash256::from_lendian(spork_hash(&good)),
    };

    let (mut sim, _) = node(false);
    let src = sim.connect(1, ConnectionType::Inbound, Permissions::NONE);
    let other = sim.connect(2, ConnectionType::OutboundFullRelay, Permissions::NONE);
    let blocks = sim.connect(3, ConnectionType::BlockRelay, Permissions::NONE);
    for p in [&src, &other, &blocks] {
        sim.handshake(p, PeerVersionMsg::v24(SIM_START));
        while sim.pass() {}
        invs(&sim, p, 0);
    }
    let deliver = |sim: &mut Sim, s: &Spork| {
        let mut payload = Vec::new();
        s.encode(&mut payload);
        sim.deliver(&src, &[("spork", payload)]);
        while sim.pass() {}
        // A peer that came before the source in the pass's shuffle announces on the next one.
        sim.pass();
    };
    deliver(&mut sim, &good);
    for p in [&src, &other, &blocks] {
        assert_eq!(invs(&sim, p, 1), [g], "peer {}", p.handle.id());
    }
    // A repeat changes nothing: no second relay.
    deliver(&mut sim, &good);
    for p in [&src, &other, &blocks] {
        assert_eq!(invs(&sim, p, 0), [], "peer {}", p.handle.id());
    }
    deliver(&mut sim, &bad);
    assert!(src.handle.disconnect_requested());
}
