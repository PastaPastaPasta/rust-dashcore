//! `MnAuthHandler` in the processor's sim.

use std::sync::Arc;

use dash_args::ArgsManager;
use dash_consensus::hash::sha256d;
use dash_evo::types::ProTxHash;
use dash_p2p::conn::Permissions;
use dash_p2p::rng::{RandSource, StreamRand};
use dash_p2p::{CommandString, ConnectionType, MAX_VERIFIED_INBOUND_PER_PROTX, PeerHandle};
use dash_p2p_core::MnAuth;
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_pkc::bls::{BlsSchemeKind, BlsSigBytes};
use dash_primitives::TxHash;
use dash_types::codec::BaseCodec;
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::logic::ProcessorOptions;
use dashd_p2p::processing::seams::{MasternodeSeams, MnListView, OperatorKeyRef};
use dashd_p2p::sim::masternode::{FakeMnList, FakeMnNet};
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, Sim, SimPeer};
use dashd_p2p::{Registry, Seams};
use tracing::Level;

use super::sign_hash;
use super::{MnAuthHandler, RelayMember};
use crate::active::key::parse;
use crate::active::{ActiveMasternode, ActiveState, OperatorKey};
use crate::meta::MasternodeMetaMan;
use crate::mnconn::deterministic_outbound;
use crate::test_log::logged;

// Public regtest v0.0.4 network-mn-0 key, also recorded by A1.
const SECRET: &str = "6fa2f4c370d6603b6e850babbcca53f4f8af3289a20189ba4ff274defebdf1bb";
const PRO: [u8; 32] = [7; 32];

fn key() -> OperatorKey {
    let args = ArgsManager::new();
    args.force_set_arg("-masternodeblsprivkey", SECRET);
    parse(&args).unwrap().unwrap()
}

fn operator(key: &OperatorKey, legacy: bool) -> OperatorKeyRef {
    let stored = key.pubkey().to_bytes(BlsSchemeKind::from_legacy(legacy));
    OperatorKeyRef {
        stored,
        stored_legacy: legacy,
        pubkey_hash: sha256d(&stored),
    }
}

/// A proTxHash `me` for which `DeterministicOutboundConnection(me, PRO)` is `me` when `ours`.
fn me(ours: bool) -> [u8; 32] {
    (1..=255u8)
        .map(|b| [b; 32])
        .find(|m| *m != PRO && (deterministic_outbound(*m, PRO) == *m) == ours)
        .unwrap()
}

struct Node {
    sim: Sim,
    meta: Arc<MasternodeMetaMan>,
    net: Arc<FakeMnNet>,
    key: OperatorKey,
}

struct Opts {
    synced: bool,
    me: Option<[u8; 32]>,
    /// Every listed masternode is a quorum relay member.
    relay_member: bool,
    /// The list, if not `PRO` alone under the test key.
    list: Option<FakeMnList>,
    /// The network core's random source, if not the sim's.
    rand: Option<Arc<dyn RandSource>>,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            synced: true,
            me: None,
            relay_member: false,
            list: None,
            rand: None,
        }
    }
}

fn node(opts: Opts) -> Node {
    let key = key();
    let list =
        Arc::new(opts.list.unwrap_or_else(|| FakeMnList::new().with(PRO, operator(&key, false))));
    let net = Arc::new(FakeMnNet::new(opts.synced, Arc::clone(&list) as _));
    let meta = Arc::new(MasternodeMetaMan::new());
    let active = opts.me.map(|pro| {
        let active = Arc::new(ActiveMasternode::new(self::key()));
        *active.st.write() = ActiveState {
            pro_tx_hash: Some(pro),
            ..ActiveState::default()
        };
        active
    });
    let relay: RelayMember = {
        let member = opts.relay_member;
        Arc::new(move |pro| member && list.operator(pro).is_some())
    };
    let mut registry = Registry::new();
    registry
        .register(Box::new(MnAuthHandler::new(Arc::clone(&meta), active).with_relay_member(relay)));
    let p = Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let id = local_identity(&p, &IdentityOptions::new()).unwrap();
    let mn = MasternodeSeams {
        net: Arc::clone(&net) as _,
        signer: None,
    };
    let sim = Sim::with_masternode(
        p,
        id,
        ProcessorOptions::default(),
        registry,
        Seams::default(),
        mn,
        |cfg| {
            if let Some(rand) = opts.rand {
                *cfg = cfg.clone().with_rand_source(rand);
            }
        },
    );
    Node {
        sim,
        meta,
        net,
        key,
    }
}

const PEER_VERSION: i32 = 70242;

impl Node {
    /// A peer at 10.0.0.`n` past the handshake.
    fn peer(&mut self, n: u8, conn_type: ConnectionType) -> SimPeer {
        self.peer_with(n, conn_type, PeerVersionMsg::v24(SIM_START))
    }

    /// As [`Node::peer`], with the peer's `version` `v`.
    fn peer_with(&mut self, n: u8, conn_type: ConnectionType, v: PeerVersionMsg) -> SimPeer {
        let peer = self.sim.connect(n, conn_type, Permissions::NONE);
        self.sim.handshake(&peer, v);
        while self.sim.pass() {}
        assert!(peer.handle.successfully_connected());
        assert_ne!(peer.handle.sent_mnauth_challenge(), [0; 32]);
        self.sim.sent(&peer);
        peer
    }

    /// The `mnauth` the masternode on the other side of `peer` sends for `pro`.
    fn mnauth(&self, peer: &PeerHandle, pro: [u8; 32]) -> Vec<u8> {
        let pk = self.key.pubkey().to_bytes(BlsSchemeKind::Basic);
        let hash = sign_hash(&pk, &peer.sent_mnauth_challenge(), !peer.is_inbound(), PEER_VERSION);
        let sig = self.key.sign(&hash, false).to_bytes(BlsSchemeKind::Basic);
        let mut out = Vec::new();
        MnAuth::new(TxHash::from(pro), BlsSigBytes::from_bytes(sig)).encode(&mut out);
        out
    }

    /// Delivers `payload` as `mnauth` from `peer` and runs the loop: the lines it logged.
    fn deliver(&mut self, peer: &SimPeer, payload: Vec<u8>) -> Vec<String> {
        self.sim.deliver(peer, &[("mnauth", payload)]);
        let sim = &mut self.sim;
        let ((), lines) = logged(|| while sim.pass() {});
        lines.into_iter().filter(|(l, _)| *l == Level::DEBUG).map(|(_, m)| m).collect()
    }

    fn auth(&mut self, peer: &SimPeer) -> Vec<String> {
        let payload = self.mnauth(&peer.handle, PRO);
        self.deliver(peer, payload)
    }
}

fn verified(peer: &SimPeer) -> Option<[u8; 32]> {
    let h = peer.handle.verified_proreg_hash();
    (h != [0; 32]).then_some(h)
}

fn has(lines: &[String], text: &str) -> bool {
    lines.iter().any(|l| l.contains(text))
}

fn valid_line(peer: &SimPeer) -> String {
    format!(
        "CMNAuth::ProcessMessage -- Valid MNAUTH for {}, peer={}",
        ProTxHash::from(PRO),
        peer.handle.id()
    )
}

// Core: evo/mnauth.cpp:51-177 at v24.0.0
#[test]
fn a_valid_inbound_mnauth_is_granted_and_logged() {
    let mut n = node(Opts::default());
    let p = n.peer(1, ConnectionType::Inbound);
    let lines = n.auth(&p);
    assert_eq!(verified(&p), Some(PRO));
    assert_eq!(p.handle.verified_pubkey_hash(), operator(&n.key, false).pubkey_hash);
    assert!(lines.contains(&valid_line(&p)), "{lines:?}");
    assert!(!has(&lines, "Misbehaving"), "{lines:?}");
    assert!(!p.handle.disconnect_requested());
    assert_eq!(n.meta.last_outbound_success(&ProTxHash::from(PRO)), 0);
    assert!(!p.handle.masternode_iqr());
    assert!(n.sim.sent(&p).is_empty());
}

// Core: evo/mnauth.cpp:65-68 at v24.0.0
#[test]
fn a_second_mnauth_scores_100() {
    let mut n = node(Opts::default());
    let p = n.peer(1, ConnectionType::Inbound);
    n.auth(&p);
    let lines = n.auth(&p);
    assert!(
        has(
            &lines,
            &format!(
                "Misbehaving: peer={} (0 -> 100) DISCOURAGE THRESHOLD EXCEEDED: duplicate mnauth",
                p.handle.id()
            )
        ),
        "{lines:?}"
    );
}

// Core: evo/mnauth.cpp:57-63 at v24.0.0
#[test]
fn an_unsynced_node_reads_nothing_and_a_synced_one_throws_on_a_bad_read() {
    let mut n = node(Opts {
        synced: false,
        ..Opts::default()
    });
    let p = n.peer(1, ConnectionType::Inbound);
    let lines = n.deliver(&p, vec![1, 2, 3]);
    assert!(!has(&lines, "Exception"), "{lines:?}");
    let lines = n.auth(&p);
    assert!(lines.is_empty() || !has(&lines, "MNAUTH"), "{lines:?}");
    assert_eq!(verified(&p), None);
    assert!(!p.handle.disconnect_requested());

    n.net.set_synced(true);
    let lines = n.deliver(&p, vec![1, 2, 3]);
    assert!(has(&lines, "Exception 'DataStream::read(): end of data"), "{lines:?}");
    let mut malleable = n.mnauth(&p.handle, PRO);
    malleable[32..].fill(0xff);
    let lines = n.deliver(&p, malleable);
    assert!(has(&lines, "Exception 'malleable BLS object"), "{lines:?}");
    assert_eq!(verified(&p), None);
    let lines = n.auth(&p);
    assert!(lines.contains(&valid_line(&p)), "{lines:?}");
}

// Core: evo/mnauth.cpp:85-91, :98-102 at v24.0.0
#[test]
fn an_unknown_or_unverified_masternode_scores_10() {
    let mut n = node(Opts::default());
    let p = n.peer(1, ConnectionType::Inbound);
    let payload = n.mnauth(&p.handle, [8; 32]);
    let lines = n.deliver(&p, payload);
    assert!(has(&lines, "(0 -> 10): missing mnauth masternode"), "{lines:?}");
    let q = n.peer(2, ConnectionType::Inbound);
    let payload = n.mnauth(&p.handle, PRO);
    let lines = n.deliver(&q, payload);
    assert!(
        has(
            &lines,
            &format!("constructed signHash for nVersion {PEER_VERSION}, peer={}", q.handle.id())
        ),
        "{lines:?}"
    );
    assert!(has(&lines, "(0 -> 10): mnauth signature verification failed"), "{lines:?}");
    assert_eq!((verified(&p), verified(&q)), (None, None));
}

// Core: evo/mnauth.cpp:127-136, :158-163; net.cpp:4514-4539 at v24.0.0
#[test]
fn old_inbounds_marked_as_probes_keep_the_new_peer_until_the_verified_cap() {
    let mut n = node(Opts {
        me: Some(me(true)),
        ..Opts::default()
    });
    let peers: Vec<SimPeer> = (1..=4).map(|i| n.peer(i, ConnectionType::Inbound)).collect();
    for (i, p) in peers.iter().enumerate().take(MAX_VERIFIED_INBOUND_PER_PROTX) {
        let lines = n.auth(p);
        assert_eq!(verified(p), Some(PRO), "peer {i}");
        for old in &peers[..i] {
            assert!(
                lines.contains(&format!(
                    "CMNAuth::ProcessMessage -- marking old inbound for dropping it later, peer={}",
                    old.handle.id()
                )),
                "{lines:?}"
            );
            assert!(old.handle.masternode_probe() && !old.handle.disconnect_requested());
        }
        assert!(!p.handle.masternode_probe());
    }
    let last = &peers[MAX_VERIFIED_INBOUND_PER_PROTX];
    let lines = n.auth(last);
    assert_eq!(verified(last), None);
    assert!(last.handle.disconnect_requested());
    assert!(!has(&lines, "Misbehaving"), "{lines:?}");
    assert!(!lines.contains(&valid_line(last)), "{lines:?}");
}

// Core: evo/mnauth.cpp:165-173 at v24.0.0
#[test]
fn a_relay_member_gets_qsendrecsigs_once() {
    let mut n = node(Opts {
        relay_member: true,
        ..Opts::default()
    });
    let p = n.peer(1, ConnectionType::Inbound);
    n.auth(&p);
    assert!(p.handle.masternode_iqr());
    n.sim.pass();
    let sent = n.sim.sent(&p);
    assert_eq!(sent, vec![(CommandString::QSENDRECSIGS, vec![1])]);

    let mut m = node(Opts {
        relay_member: true,
        ..Opts::default()
    });
    let q = m.peer(1, ConnectionType::Inbound);
    q.handle.set_masternode_iqr(true);
    let lines = m.auth(&q);
    assert!(lines.contains(&valid_line(&q)), "{lines:?}");
    m.sim.pass();
    assert!(m.sim.sent(&q).is_empty());

    let mut plain = node(Opts::default());
    let r = plain.peer(1, ConnectionType::Inbound);
    plain.auth(&r);
    assert!(!r.handle.masternode_iqr());
}

// Core: evo/mnauth.cpp:104-111, :137-144 at v24.0.0
#[test]
fn a_dropped_outbound_keeps_its_outbound_success() {
    let mut n = node(Opts {
        me: Some(me(false)),
        ..Opts::default()
    });
    n.sim.clock.set_mock_time(SIM_START + 5);
    let old = n.peer(1, ConnectionType::Inbound);
    n.auth(&old);
    let new = n.peer(2, ConnectionType::OutboundFullRelay);
    let lines = n.auth(&new);
    assert!(
        lines.contains(&format!(
            "CMNAuth::ProcessMessage -- dropping new outbound, peer={}",
            new.handle.id()
        )),
        "{lines:?}"
    );
    assert!(new.handle.disconnect_requested() && verified(&new).is_none());
    assert!(!old.handle.disconnect_requested());
    assert_eq!(n.meta.last_outbound_success(&ProTxHash::from(PRO)), SIM_START + 5);

    let probe = n.sim.connect_probe(3);
    n.sim.handshake(&probe, PeerVersionMsg::v24(SIM_START));
    while n.sim.pass() {}
    n.sim.clock.set_mock_time(SIM_START + 9);
    let lines = n.auth(&probe);
    assert!(
        lines.contains(&format!(
            "CMNAuth::ProcessMessage -- Masternode probe successful for {}, disconnecting. peer={}",
            ProTxHash::from(PRO),
            probe.handle.id()
        )),
        "{lines:?}"
    );
    assert!(probe.handle.disconnect_requested() && verified(&probe).is_none());
    assert_eq!(n.meta.last_outbound_success(&ProTxHash::from(PRO)), SIM_START + 9);
}

// Core: evo/mnauth.cpp:117-120, :137-144 at v24.0.0
#[test]
fn the_walk_stops_after_the_new_peer_is_dropped() {
    let mut n = node(Opts {
        me: Some(me(false)),
        ..Opts::default()
    });
    let inbound = n.peer(1, ConnectionType::Inbound);
    let outbound = n.peer(2, ConnectionType::OutboundFullRelay);
    for p in [&inbound, &outbound] {
        assert!(n.sim.core.try_mark_verified(p.handle.id(), PRO, [3; 32]));
    }
    let new = n.peer(3, ConnectionType::OutboundFullRelay);
    let lines = n.auth(&new);
    assert!(new.handle.disconnect_requested(), "{lines:?}");
    assert!(!outbound.handle.disconnect_requested(), "{lines:?}");
    assert!(!has(&lines, "dropping old outbound"), "{lines:?}");

    let mut m = node(Opts {
        me: Some(me(false)),
        ..Opts::default()
    });
    let outbound = m.peer(1, ConnectionType::OutboundFullRelay);
    let inbound = m.peer(2, ConnectionType::Inbound);
    for p in [&outbound, &inbound] {
        assert!(m.sim.core.try_mark_verified(p.handle.id(), PRO, [3; 32]));
    }
    let new = m.peer(3, ConnectionType::OutboundFullRelay);
    m.auth(&new);
    assert!(outbound.handle.disconnect_requested(), "an earlier drop stays applied");
    assert!(new.handle.disconnect_requested());
}

// Core: evo/mnauth.cpp:146-149 at v24.0.0
#[test]
fn a_node_without_a_protx_drops_the_new_connection() {
    let mut n = node(Opts::default());
    let old = n.peer(1, ConnectionType::Inbound);
    n.auth(&old);
    let new = n.peer(2, ConnectionType::Inbound);
    let lines = n.auth(&new);
    assert!(has(&lines, "has already verified as peer"), "{lines:?}");
    assert!(new.handle.disconnect_requested() && !old.handle.disconnect_requested());
}

// Core: net_processing.cpp:4388-4396 at v24.0.0
#[test]
fn a_probe_flip_reaches_the_first_message_gate() {
    let mut n = node(Opts::default());
    let p = n.peer(1, ConnectionType::Inbound);
    p.handle.set_masternode_probe(true);
    n.sim.deliver(&p, &[("ping", 1u64.to_le_bytes().to_vec())]);
    while n.sim.pass() {}
    assert!(p.handle.disconnect_requested());
}

/// The port of Core's `verified_inbound_capped_per_protx`.
// Core: test/evo_mnauth_tests.cpp:47-95; net.cpp:4514-4539 at v24.0.0
#[test]
fn verified_inbound_capped_per_protx() {
    let mut n = node(Opts::default());
    let core = Arc::clone(&n.sim.core);
    let (attacker, other, pubkey_hash) = ([1; 32], [2; 32], [3; 32]);
    assert!(!core.try_mark_verified(1337, attacker, pubkey_hash));
    let legs: Vec<SimPeer> = (1..=MAX_VERIFIED_INBOUND_PER_PROTX as u8)
        .map(|i| n.peer(i, ConnectionType::Inbound))
        .collect();
    for leg in &legs {
        assert!(core.try_mark_verified(leg.handle.id(), attacker, pubkey_hash));
        assert_eq!(leg.handle.verified_proreg_hash(), attacker);
        assert_eq!(leg.handle.verified_pubkey_hash(), pubkey_hash);
    }
    let refused = n.peer(10, ConnectionType::Inbound);
    assert!(!core.try_mark_verified(refused.handle.id(), attacker, pubkey_hash));
    assert_eq!(refused.handle.verified_proreg_hash(), [0; 32]);
    assert_eq!(refused.handle.verified_pubkey_hash(), [0; 32]);
    let other_node = n.peer(11, ConnectionType::Inbound);
    assert!(core.try_mark_verified(other_node.handle.id(), other, pubkey_hash));
    let outbound = n.peer(12, ConnectionType::OutboundFullRelay);
    assert!(core.try_mark_verified(outbound.handle.id(), attacker, pubkey_hash));
    core.disconnect(legs[0].handle.id(), dash_p2p::Why::Processor("test"));
    assert!(core.try_mark_verified(refused.handle.id(), attacker, pubkey_hash));
    assert_eq!(refused.handle.verified_proreg_hash(), attacker);
}

/// The sim's random stream, except that every 32-byte draw while armed is the given challenge
/// (`GetRandBytes` for `sentMNAuthChallenge`, net_processing.cpp:1572-1575 at v24.0.0).
struct ArmedRand {
    inner: StreamRand,
    next: parking_lot::Mutex<Option<[u8; 32]>>,
}

impl RandSource for ArmedRand {
    fn rand_bytes(&self, out: &mut [u8], strong: bool) {
        if out.len() == 32
            && !strong
            && let Some(challenge) = *self.next.lock()
        {
            out.copy_from_slice(&challenge);
            return;
        }
        self.inner.rand_bytes(out, strong);
    }
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap()).collect()
}

/// A uint256 as RPC prints it, in internal byte order.
fn rpc_hash(text: &str) -> [u8; 32] {
    let mut h: [u8; 32] = unhex(text).try_into().unwrap();
    h.reverse();
    h
}

/// `line` with each Core peer id replaced by the sim's, and its category prefix dropped.
fn normalized(line: &str, ids: &[(u64, i64)]) -> String {
    let mut out = line.split_once("] ").map_or(line, |(_, rest)| rest).to_owned();
    for (core, sim) in ids {
        for sep in ["peer=", "peer "] {
            out = out.replace(&format!("{sep}{core},"), &format!("{sep}<{sim}>,"));
            if out.ends_with(&format!("{sep}{core}")) {
                out.truncate(out.len() - core.to_string().len());
                out.push_str(&format!("<{sim}>"));
            }
            out = out.replace(&format!("{sep}{core} "), &format!("{sep}<{sim}> "));
        }
    }
    out
}

const INTAKE_LINES: [&str; 3] =
    ["CMNAuth::ProcessMessage", "Misbehaving:", "ProcessMessages(mnauth"];

/// Replays `contrib/node/am1-captures/mnauth.py --mode in` (Core v24.0.0, regtest v0.0.4)
/// through the real handler: the recorded list entry and scheme, our proTxHash, the receiver's
/// challenge, the peer's version and services, and the earlier peers in table order.
// Core: evo/mnauth.cpp:51-177; net.cpp:4514-4539 at v24.0.0
#[test]
fn core_v24_mnauth_intake_replays_through_the_handler() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/fixtures/mnauth/core-v24-in.json")).unwrap();
    let records = fixture["records"].as_array().unwrap();
    assert_eq!(records.len(), 10);
    for r in records {
        let name = r["name"].as_str().unwrap();
        assert!(r.get("missing").is_none(), "{name}: not recorded");
        let op = &r["operator"];
        let stored: [u8; 48] = unhex(op["stored"].as_str().unwrap()).try_into().unwrap();
        let stored_legacy = op["stored_scheme"] == "legacy";
        let sender = rpc_hash(op["pro_tx_hash"].as_str().unwrap());
        let key = OperatorKeyRef {
            stored,
            stored_legacy,
            pubkey_hash: sha256d(&stored),
        };
        let list = FakeMnList::new()
            .with(sender, key)
            .with_bls_legacy_scheme(r["process_scheme"] == "legacy");
        let observed = &r["observed"];
        let qsendrecsigs: Vec<Vec<u8>> = observed["qsendrecsigs"]
            .as_array()
            .map(|a| a.iter().map(|q| unhex(q.as_str().unwrap())).collect())
            .unwrap_or_default();
        let rand = Arc::new(ArmedRand {
            inner: StreamRand::with_key([0; 32]),
            next: parking_lot::Mutex::new(None),
        });
        let mut n = node(Opts {
            synced: r["synced"].as_bool().unwrap(),
            me: Some(rpc_hash(r["me"].as_str().unwrap())),
            relay_member: !qsendrecsigs.is_empty(),
            list: Some(list),
            rand: Some(Arc::clone(&rand) as _),
        });

        let mut ids = Vec::new();
        let mut pre = Vec::new();
        for (i, p) in r["pre_peers"].as_array().unwrap().iter().enumerate() {
            let conn_type = if p["inbound"].as_bool().unwrap() {
                ConnectionType::Inbound
            } else {
                ConnectionType::OutboundFullRelay
            };
            let peer = n.peer(10 + i as u8, conn_type);
            if let Some(pro) = p["verified_proregtx_hash"].as_str() {
                let pubkey = rpc_hash(p["verified_pubkey_hash"].as_str().unwrap());
                assert!(n.sim.core.try_mark_verified(peer.handle.id(), rpc_hash(pro), pubkey));
            }
            ids.push((p["id"].as_u64().unwrap(), peer.handle.id()));
            pre.push(peer);
        }

        let challenge: [u8; 32] = unhex(r["sent_challenge"].as_str().unwrap()).try_into().unwrap();
        *rand.next.lock() = Some(challenge);
        let conn_type = if r["direction"] == "inbound" {
            ConnectionType::Inbound
        } else {
            ConnectionType::OutboundFullRelay
        };
        let mut v = PeerVersionMsg::v24(SIM_START);
        v.version = r["peer_version"].as_i64().unwrap() as i32;
        v.services = r["peer_services"].as_u64().unwrap();
        let peer = n.peer_with(1, conn_type, v);
        *rand.next.lock() = None;
        assert_eq!(peer.handle.sent_mnauth_challenge(), challenge, "{name}: challenge");
        ids.push((r["peer_id"].as_u64().unwrap(), peer.handle.id()));

        let lines = n.deliver(&peer, unhex(r["payload"].as_str().unwrap()));
        let got: Vec<String> = lines
            .iter()
            .filter(|l| INTAKE_LINES.iter().any(|p| l.starts_with(p)))
            .cloned()
            .collect();
        let want: Vec<String> = observed["log"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| normalized(l.as_str().unwrap(), &ids))
            .collect();
        let got: Vec<String> = got.iter().map(|l| normalized(l, &[])).collect();
        let want: Vec<String> = want
            .iter()
            .map(|l| {
                let mut l = l.clone();
                for (_, sim) in &ids {
                    l = l.replace(&format!("<{sim}>"), &sim.to_string());
                }
                l
            })
            .collect();
        assert_eq!(got, want, "{name}: log");

        let after = &observed["peer"];
        let want_verified =
            after.get("verified_proregtx_hash").and_then(|h| h.as_str()).map(rpc_hash);
        assert_eq!(verified(&peer), want_verified, "{name}: verified_proregtx_hash");
        if !after.is_null() {
            let want_pubkey = after["verified_pubkey_hash"].as_str().map_or([0; 32], rpc_hash);
            assert_eq!(
                peer.handle.verified_pubkey_hash(),
                want_pubkey,
                "{name}: verified_pubkey_hash"
            );
        }
        if let Some(score) = after.get("banscore").and_then(|s| s.as_i64()) {
            let rust = got
                .iter()
                .filter_map(|l| l.split_once(" -> ").map(|(_, rest)| rest))
                .filter_map(|rest| rest.split(')').next()?.parse::<i64>().ok())
                .next_back()
                .unwrap_or(0);
            assert_eq!(rust, score, "{name}: banscore");
        }
        if after.is_null() && r["kind"] == "core-masternode" {
            assert!(peer.handle.disconnect_requested(), "{name}: dropped");
        }
        if let Some(survivors) = observed.get("pre_peers").and_then(|p| p.as_array()) {
            for (p, was) in pre.iter().zip(survivors) {
                assert_eq!(p.handle.disconnect_requested(), was.is_null(), "{name}: earlier peer");
            }
        }
        n.sim.pass();
        let sent: Vec<Vec<u8>> = n
            .sim
            .sent(&peer)
            .into_iter()
            .filter(|(c, _)| *c == CommandString::QSENDRECSIGS)
            .map(|(_, p)| p)
            .collect();
        assert_eq!(sent, qsendrecsigs, "{name}: qsendrecsigs");
        assert_eq!(peer.handle.masternode_iqr(), !qsendrecsigs.is_empty(), "{name}: iqr");
    }
}
