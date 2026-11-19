//! P6b quorum-rotation-info serving boundaries in the processor simulation.

use std::sync::{Arc, Mutex};

use dash_p2p::ConnectionType;
use dash_p2p::conn::Permissions;
use dash_p2p_core::{GetQrInfo, ProtocolVersion};
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_primitives::BlockHash;
use dash_types::Numeric;
use dash_types::codec::BaseCodec;
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::logic::ProcessorOptions;
use dashd_p2p::processing::seams::{EvoServe, QrInfoError};
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, Sim, SimPeer};
use dashd_p2p::{Registry, Seams};

fn sim(seams: Seams) -> Sim {
    let params =
        Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let identity = local_identity(&params, &IdentityOptions::new()).unwrap();
    Sim::with(params, identity, ProcessorOptions::default(), Registry::new(), seams, |_| {})
}

fn peer(sim: &mut Sim, n: u8, version: i32) -> SimPeer {
    let peer = sim.connect(n, ConnectionType::Inbound, Permissions::NONE);
    sim.handshake(
        &peer,
        PeerVersionMsg {
            version,
            ..PeerVersionMsg::v24(SIM_START)
        },
    );
    sim.sent(&peer);
    peer
}

fn request(base_count: usize) -> Vec<u8> {
    let request = GetQrInfo::new(vec![BlockHash::ZERO; base_count], BlockHash::ZERO, false);
    let mut payload = Vec::new();
    request.encode(&mut payload);
    payload
}

fn score(sim: &Sim, peer: &SimPeer) -> i32 {
    sim.shared.misbehavior(peer.handle.id()).unwrap().0
}

#[test]
fn base_hashes_at_limit_are_decoded_and_oversize_scores() {
    let mut sim = sim(Seams::default());
    let at_limit = peer(&mut sim, 1, 70242);
    sim.admitted(&at_limit, "getqrinfo", request(4096)).unwrap();
    assert_eq!(score(&sim, &at_limit), 0);
    assert!(sim.sent(&at_limit).is_empty());

    let over_limit = peer(&mut sim, 2, 70242);
    sim.admitted(&over_limit, "getqrinfo", request(4097)).unwrap();
    assert_eq!(score(&sim, &over_limit), 100);
    assert!(sim.sent(&over_limit).is_empty());
}

#[test]
fn malformed_getqrinfo_scores_without_a_reply() {
    let mut sim = sim(Seams::default());
    let peer = peer(&mut sim, 1, 70242);
    sim.admitted(&peer, "getqrinfo", vec![0]).unwrap();
    assert_eq!(score(&sim, &peer), 100);
    assert!(sim.sent(&peer).is_empty());
}

#[derive(Clone)]
struct LegacyProbe(Arc<Mutex<Vec<(i32, bool)>>>);

impl EvoServe for LegacyProbe {
    fn qrinfo(
        &self,
        _request: &GetQrInfo,
        version: ProtocolVersion,
        legacy: bool,
    ) -> Result<Vec<u8>, QrInfoError> {
        self.0.lock().unwrap().push((version.get(), legacy));
        Err(QrInfoError::Unavailable("not available".into()))
    }
}

struct RefusalProbe(QrInfoError);

impl EvoServe for RefusalProbe {
    fn qrinfo(
        &self,
        _request: &GetQrInfo,
        _version: ProtocolVersion,
        _legacy: bool,
    ) -> Result<Vec<u8>, QrInfoError> {
        Err(self.0.clone())
    }
}

fn reply_bytes() -> Vec<u8> {
    vec![0xde, 0xad, 0xbe, 0xef]
}

struct SuccessProbe;

impl EvoServe for SuccessProbe {
    fn qrinfo(
        &self,
        _request: &GetQrInfo,
        _version: ProtocolVersion,
        _legacy: bool,
    ) -> Result<Vec<u8>, QrInfoError> {
        Ok(reply_bytes())
    }
}

#[test]
fn successful_build_sends_the_reply_unchanged() {
    let expected = reply_bytes();
    let mut sim = sim(Seams {
        evo: Arc::new(SuccessProbe),
        ..Seams::default()
    });
    let peer = peer(&mut sim, 1, 70242);
    sim.admitted(&peer, "getqrinfo", request(0)).unwrap();
    let sent = sim.sent(&peer);
    assert_eq!(score(&sim, &peer), 0);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0.as_str(), "qrinfo");
    assert_eq!(sent[0].1, expected);
}

#[test]
fn refusal_outcomes_match_core_scores() {
    let mut unavailable = sim(Seams {
        evo: Arc::new(RefusalProbe(QrInfoError::Unavailable("missing".into()))),
        ..Seams::default()
    });
    let unavailable_peer = peer(&mut unavailable, 1, 70242);
    unavailable.admitted(&unavailable_peer, "getqrinfo", request(0)).unwrap();
    assert_eq!(score(&unavailable, &unavailable_peer), 0);
    assert!(unavailable.sent(&unavailable_peer).is_empty());

    let mut refused = sim(Seams {
        evo: Arc::new(RefusalProbe(QrInfoError::Refused("invalid".into()))),
        ..Seams::default()
    });
    let refused_peer = peer(&mut refused, 1, 70242);
    refused.admitted(&refused_peer, "getqrinfo", request(0)).unwrap();
    assert_eq!(score(&refused, &refused_peer), 1);
    assert!(refused.sent(&refused_peer).is_empty());
}

#[test]
fn legacy_construction_follows_the_common_version() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seams = Seams {
        evo: Arc::new(LegacyProbe(Arc::clone(&seen))),
        ..Seams::default()
    };
    let mut sim = sim(seams);
    let unversioned = peer(&mut sim, 1, 70228);
    sim.admitted(&unversioned, "getqrinfo", request(0)).unwrap();
    let old = peer(&mut sim, 2, 70235);
    sim.admitted(&old, "getqrinfo", request(0)).unwrap();
    let current = peer(&mut sim, 3, 70236);
    sim.admitted(&current, "getqrinfo", request(0)).unwrap();
    assert_eq!(*seen.lock().unwrap(), [(70228, true), (70235, true), (70236, false)]);
    assert!(
        sim.sent(&unversioned).is_empty()
            && sim.sent(&old).is_empty()
            && sim.sent(&current).is_empty()
    );
}
