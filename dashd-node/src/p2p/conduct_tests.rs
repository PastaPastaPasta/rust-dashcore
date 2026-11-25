//! FR-AMN-04: an active-masternode rdashd as the peer a Core masternode judges in
//! `ActiveDKGSession::VerifyConnectionAndMinProtoVersions`. rdashd is a sim over the node's own
//! products ([`net_config`], [`masternode_seams`], AM1's [`MnAuthSigning`], [`node_registry`]);
//! the judging side is a second sim that runs the node's `CMNAuth` handler and its own
//! [`MnAuthSigning`], and the two exchange their real frames.
//! The signer and the verifier here are both rdashd code; the MNAUTH signHash's parity with Core
//! is pinned by the handler's Core-vector tests (`dashd-masternode/src/mnauth/handler_tests.rs`,
//! `core_v24_mnauth_intake_replays_through_the_handler`).
// Core: active/dkgsession.cpp:255-319 at v24.0.0

use std::sync::Arc;

use dash_args::ArgsManager;
use dash_consensus::BlsCtx;
use dash_consensus::hash::sha256d;
use dash_dml::testkit::dmn_with;
use dash_dml::{MnList, MnType};
use dash_p2p::{ConnectionType, NetAddr, Permissions, Service, ServiceFlags};
use dash_p2p_core::ProtocolVersion;
use dash_pkc::bls::BlsSchemeKind;
use dash_primitives::{AddrV1, NetInfo, NetInfoV1, ServiceV1};
use dashd_masternode::active::ActiveMasternode;
use dashd_masternode::active::state::{LocalFacts, TipCtx};
use dashd_masternode::mnauth::MnAuthSigning;
use dashd_masternode::sync::NodeSyncNotifier;
use dashd_masternode::tip::{TipListRef, TipMnList};
use dashd_p2p::fingerprint::PROTOCOL_VERSION;
use dashd_p2p::processing::seams::{NoPlatformBans, OperatorKeyRef};
use dashd_p2p::processing::serve_mnlistdiff::NoEvoServe;
use dashd_p2p::sim::masternode::{FakeMnList, FakeMnNet};
use dashd_p2p::sim::{SIM_START, Sim, SimPeer, VirtualClock};

use super::amb::regtest;
use super::mnauth_wire_tests::active_as;
use super::*;
use crate::init::checks::parameter_interaction;
use crate::init::interaction::init_parameter_interaction;
use crate::init::test_args;
use crate::ui::Ui;

/// Our proTxHash.
const PRO: [u8; 32] = [7; 32];

/// The judging masternode's proTxHash.
const JUDGE: [u8; 32] = [8; 32];

/// The judging masternode's operator key, built at runtime.
fn judge_secret() -> String {
    "03".repeat(32)
}

/// Frames as (command, payload).
type Frames = Vec<(String, Vec<u8>)>;

/// What a masternode must run with (init.cpp:1385-1431), the key built at runtime.
fn masternode_argv() -> Vec<String> {
    let key = format!("-masternodeblsprivkey={}", "02".repeat(32));
    ["-peerbloomfilters=1", "-txindex=1"].into_iter().map(String::from).chain([key]).collect()
}

struct Quiet;

impl NodeSyncNotifier for Quiet {
    fn sync_reset(&self) {}

    fn sync_finished(&self) {}
}

/// An operator key loaded from `secret` (hex).
fn loaded(secret: &str) -> Arc<ActiveMasternode> {
    let args = ArgsManager::new();
    args.force_set_arg("-masternodeblsprivkey", secret);
    dashd_masternode::active::load(&args).unwrap().unwrap()
}

/// An active masternode loaded from `secret` (hex) and seated as `pro`.
fn seated(pro: [u8; 32], secret: &str) -> Arc<ActiveMasternode> {
    let active = loaded(secret);
    let mut list = MnList::default_pre_dip3();
    list.insert_for_tests(mn_entry(&active, pro));
    let service = Service::new(NetAddr::ipv4([127, 0, 0, 1]), 19899);
    let tip = TipCtx {
        height: 1000,
        dip3_enforced: true,
        require_routable: false,
    };
    let local = LocalFacts {
        listen: true,
        require_routable: false,
        local_toward_v4: None,
        loopback_listen: service,
        peers: Vec::new(),
        valid: Vec::new(),
        self_connect_ok: false,
    };
    active.init(&tip, &list, &local);
    assert_eq!(active.pro_tx_hash(), Some(pro));
    active
}

/// `pro` operated by `active`'s key, stored under the basic scheme.
fn mn_entry(active: &ActiveMasternode, pro: [u8; 32]) -> Arc<dash_dml::Dmn> {
    let service = Service::new(NetAddr::ipv4([127, 0, 0, 1]), 19899);
    dmn_with(pro[0], MnType::Regular, |dmn, cold| {
        dmn.pro_tx_hash = pro.into();
        cold.version = 2;
        cold.pub_key_operator =
            dash_dml::OperatorKey::from_value(active.pubkey(), BlsSchemeKind::Basic);
        cold.net_info = Arc::new(NetInfo::Legacy(NetInfoV1::new(ServiceV1::new(
            AddrV1::from_bytes(service.addr.v1_bytes()),
            service.port,
        ))));
    })
}

/// The basic-scheme operator key `active` signs with, as a list stores it.
fn key_ref(active: &ActiveMasternode) -> OperatorKeyRef {
    let stored = active.pubkey().to_bytes(BlsSchemeKind::Basic);
    OperatorKeyRef {
        stored,
        stored_legacy: false,
        pubkey_hash: sha256d(&stored),
    }
}

/// A basic-scheme tip with an empty list: what a signer reads for its key serialization.
fn scheme_tip() -> TipMnList {
    TipMnList::fixed(TipListRef {
        list: Arc::new(MnList::default_pre_dip3()),
        bls: BlsCtx::BASIC,
    })
}

/// The published tip: basic scheme, with [`JUDGE`] under the judging masternode's key so rdashd
/// can verify the judge's MNAUTH.
fn tip() -> TipMnList {
    let judge = loaded(&judge_secret());
    let mut list = MnList::default_pre_dip3();
    list.insert_for_tests(mn_entry(&judge, JUDGE));
    TipMnList::fixed(TipListRef {
        list: Arc::new(list),
        bls: BlsCtx::BASIC,
    })
}

/// rdashd as an active masternode, synced, found in the list as [`PRO`]: init's parameter
/// interaction and step 2 over the masternode arguments, the core's config and identity from
/// [`net_config`], step 7c's seams with AM1's signer, the node's [`node_registry`] (platform ban
/// and `CMNAuth`), and step 12's accept refusal.
fn rdashd() -> Sim {
    let argv = masternode_argv();
    let args = test_args(&argv.iter().map(String::as_str).collect::<Vec<_>>());
    let logger = Arc::new(Logger::new());
    init_parameter_interaction(&args, &logger).unwrap();
    let net = parameter_interaction(&Ui::new(logger), &args).unwrap();
    let role = MasternodeRole::from_args(&args);
    assert!(role.active());
    let (cfg, ident) = net_config(&args, &regtest(), &net, &role).unwrap();
    let identity = local_identity(&cfg.params, &ident).unwrap();
    let sync = Arc::new(MasternodeSync::new(VirtualClock::at(SIM_START), Box::new(Quiet)));
    sync.switch_to_next_asset();
    let active = active_as(PRO);
    let published = tip();
    let signer = Arc::new(MnAuthSigning::new(Arc::clone(&active), published.clone()));
    let quorum_nodes = Arc::new(QuorumNodes::default());
    let meta = Arc::new(MasternodeMetaMan::new());
    let mnauth = mnauth_handler(&meta, Some(active), &quorum_nodes);
    let registry = node_registry(Box::new(NoEvoServe), Arc::new(NoPlatformBans), Some(mnauth));
    let mn =
        masternode_seams(&role, published, Arc::clone(&sync), &quorum_nodes, Some(signer)).unwrap();
    Sim::with_parts_registry(cfg, identity, refuse_unsynced(&role, &sync), mn, registry)
}

/// Our operator key as the judging side's list holds it.
fn operator() -> OperatorKeyRef {
    let stored = active_as(PRO).pubkey().to_bytes(BlsSchemeKind::Basic);
    OperatorKeyRef {
        stored,
        stored_legacy: false,
        pubkey_hash: sha256d(&stored),
    }
}

/// The judging Core masternode: synced, [`PRO`] in its list under our key, the node's `CMNAuth`
/// handler over `meta`, and [`MnAuthSigning`] for [`JUDGE`] so both sides send MNAUTH.
fn judge_node(meta: &Arc<MasternodeMetaMan>) -> Sim {
    let judge = seated(JUDGE, &judge_secret());
    let list = Arc::new(FakeMnList::new().with(PRO, operator()));
    let quorum_nodes = Arc::new(QuorumNodes::default());
    let mnauth = mnauth_handler(meta, Some(Arc::clone(&judge)), &quorum_nodes);
    let registry = node_registry(Box::new(NoEvoServe), Arc::new(NoPlatformBans), Some(mnauth));
    let p = regtest();
    let id = local_identity(&p, &IdentityOptions::new()).unwrap();
    let mn = MasternodeSeams {
        net: Arc::new(FakeMnNet::new(true, list)),
        signer: Some(Arc::new(MnAuthSigning::new(judge, scheme_tip()))),
    };
    Sim::with_masternode(p, id, ProcessorOptions::default(), registry, Seams::default(), mn, |_| {})
}

/// Moves each side's frames to the other until both are quiet; returns what `a` sent.
fn wire(a: &mut Sim, ap: &SimPeer, b: &mut Sim, bp: &SimPeer) -> Frames {
    let mut from_a = Vec::new();
    for _ in 0..32 {
        while a.pass() {}
        while b.pass() {}
        let to_b: Vec<_> = a.sent(ap).into_iter().map(|(c, p)| (c.to_string(), p)).collect();
        let to_a: Vec<_> = b.sent(bp).into_iter().map(|(c, p)| (c.to_string(), p)).collect();
        if to_b.is_empty() && to_a.is_empty() {
            return from_a;
        }
        for (cmd, payload) in &to_b {
            b.deliver(bp, &[(cmd.as_str(), payload.clone())]);
        }
        for (cmd, payload) in to_a {
            a.deliver(ap, &[(cmd.as_str(), payload)]);
        }
        from_a.extend(to_b);
    }
    panic!("the two ends never went quiet");
}

/// The one `version` payload rdashd sent.
fn version_payload(frames: &[(String, Vec<u8>)]) -> &[u8] {
    let mut versions = frames.iter().filter(|(c, _)| c == "version");
    let (_, payload) = versions.next().expect("a version");
    assert!(versions.next().is_none());
    payload
}

/// `nVersion` from the one `version` rdashd sent.
fn sent_version(frames: &[(String, Vec<u8>)]) -> i32 {
    let payload = version_payload(frames);
    i32::from_le_bytes(payload[..4].try_into().unwrap())
}

fn take(payload: &[u8], i: &mut usize, n: usize) {
    assert!(payload.len() >= *i + n, "version payload ended early");
    *i += n;
}

fn compact_size(payload: &[u8], i: &mut usize) -> usize {
    let first = payload[*i];
    *i += 1;
    match first {
        0xfd => {
            let n = u16::from_le_bytes(payload[*i..*i + 2].try_into().unwrap());
            *i += 2;
            usize::from(n)
        }
        0xfe => {
            let n = u32::from_le_bytes(payload[*i..*i + 4].try_into().unwrap());
            *i += 4;
            usize::try_from(n).unwrap()
        }
        0xff => {
            let n = u64::from_le_bytes(payload[*i..*i + 8].try_into().unwrap());
            *i += 8;
            usize::try_from(n).unwrap()
        }
        n => usize::from(n),
    }
}

/// The `m_masternode_connection` byte `PushNodeVersion` appends.
// Core: net_processing.cpp:1583-1586 at v24.0.0
fn masternode_flag_byte(payload: &[u8]) -> u8 {
    let mut i = 0;
    take(payload, &mut i, 4 + 8 + 8);
    take(payload, &mut i, 8 + 16 + 2);
    take(payload, &mut i, 8 + 16 + 2);
    take(payload, &mut i, 8);
    let ua = compact_size(payload, &mut i);
    take(payload, &mut i, ua);
    take(payload, &mut i, 4 + 1 + 32);
    let flag = payload[i];
    i += 1;
    assert_eq!(i, payload.len(), "m_masternode_connection is the last version byte");
    assert!(flag <= 1, "the flag is a bool byte: {flag}");
    flag
}

/// How rdashd and the judging node are connected.
#[derive(Clone, Copy, Debug)]
enum Link {
    /// The judge dials us (we see an inbound connection): a plain, masternode or probe one.
    Inbound {
        masternode: bool,
        probe: bool,
    },
    /// We dial the judge: a plain, masternode or probe connection.
    Outbound {
        masternode: bool,
        probe: bool,
    },
}

impl Link {
    /// Whether the link is a masternode connection, probe or not.
    fn masternode(self) -> bool {
        match self {
            Link::Inbound {
                masternode,
                ..
            }
            | Link::Outbound {
                masternode,
                ..
            } => masternode,
        }
    }
}

const LINKS: [Link; 6] = [
    Link::Inbound {
        masternode: false,
        probe: false,
    },
    Link::Inbound {
        masternode: true,
        probe: false,
    },
    Link::Inbound {
        masternode: true,
        probe: true,
    },
    Link::Outbound {
        masternode: false,
        probe: false,
    },
    Link::Outbound {
        masternode: true,
        probe: false,
    },
    Link::Outbound {
        masternode: true,
        probe: true,
    },
];

/// Peer 1 on the dialling side, as Core's `OpenMasternodeConnection` opens it.
fn dial(sim: &Sim, masternode: bool, probe: bool) -> SimPeer {
    if probe {
        return sim.connect_probe(1);
    }
    let service = Service::new(NetAddr::ipv4([10, 0, 0, 1]), 19899);
    sim.attach_at(
        service,
        ConnectionType::OutboundFullRelay,
        Permissions::NONE,
        ServiceFlags::NODE_NETWORK,
        masternode,
        false,
    )
}

/// rdashd and the judging node linked by `link` and run until quiet: rdashd's sim and peer, the
/// judge's sim and peer, and what rdashd sent.
fn linked(link: Link, meta: &Arc<MasternodeMetaMan>) -> (Sim, SimPeer, Sim, SimPeer, Frames) {
    let (mut us, mut judge) = (rdashd(), judge_node(meta));
    let (ours, theirs) = match link {
        Link::Inbound {
            masternode,
            probe,
        } => {
            let theirs = dial(&judge, masternode, probe);
            let ours = us.accept_from(1, NetAddr::ipv4([20, 1, 1, 1]), Permissions::NONE).unwrap();
            (ours, theirs)
        }
        Link::Outbound {
            masternode,
            probe,
        } => {
            let ours = dial(&us, masternode, probe);
            let theirs =
                judge.accept_from(1, NetAddr::ipv4([20, 1, 1, 1]), Permissions::NONE).unwrap();
            (ours, theirs)
        }
    };
    let sent = wire(&mut us, &ours, &mut judge, &theirs);
    (us, ours, judge, theirs, sent)
}

/// Rules out "does not have min proto version" (active/dkgsession.cpp:298-301).
// Core: version.h:14, :23; net_processing.cpp:1577-1584, :4057 at v24.0.0
#[test]
fn a7_version_meets_the_masternode_minimum_on_every_link() {
    let min = ProtocolVersion::MIN_MASTERNODE.get();
    assert_eq!(min, 70242);
    assert!(PROTOCOL_VERSION.get() >= min);
    for link in LINKS {
        let meta = Arc::new(MasternodeMetaMan::new());
        let (_us, ours, _judge, theirs, sent) = linked(link, &meta);
        assert!(ours.handle.successfully_connected(), "{link:?}");
        let version = sent_version(&sent);
        assert!(version >= min, "{link:?}: {version}");
        let recorded = theirs.handle.version();
        assert!(recorded >= min, "{link:?}: {recorded}");
    }
}

fn pro() -> dash_evo::types::ProTxHash {
    dash_evo::types::ProTxHash::from(PRO)
}

fn lines_of(f: impl FnOnce()) -> Vec<String> {
    let ((), lines) = crate::test_log::logged(f);
    lines.into_iter().map(|(_, m)| m).collect()
}

/// The `mnauth` rdashd sent: its proRegTxHash.
fn sent_mnauth(frames: &[(String, Vec<u8>)]) -> [u8; 32] {
    let mut mnauth = frames.iter().filter(|(c, _)| c == "mnauth");
    let (_, payload) = mnauth.next().expect("an mnauth");
    assert!(mnauth.next().is_none());
    assert_eq!(payload.len(), 32 + 96);
    payload[..32].try_into().unwrap()
}

/// Rules out "failed to connect to it too many times" (active/dkgsession.cpp:310-313).
// Core: evo/mnauth.cpp:19-49, :95-110; net_processing.cpp:4214-4216; masternode/meta.h:70-71
#[test]
fn a7_an_inbound_probe_gets_a_valid_mnauth_and_clears_failed_outbound() {
    let meta = Arc::new(MasternodeMetaMan::new());
    while !meta.outbound_failed_too_many_times(&pro()) {
        meta.set_last_outbound_attempt(&pro(), SIM_START);
    }
    let mut out = None;
    let lines = lines_of(|| {
        out = Some(linked(
            Link::Inbound {
                masternode: true,
                probe: true,
            },
            &meta,
        ))
    });
    let (_us, ours, _judge, theirs, sent) = out.unwrap();
    assert!(!ours.handle.masternode_probe(), "a probe on the judge's side only");
    assert_eq!(sent_mnauth(&sent), PRO);
    let probed = format!(
        "CMNAuth::ProcessMessage -- Masternode probe successful for {}, disconnecting. peer={}",
        pro(),
        theirs.handle.id()
    );
    assert!(lines.contains(&probed), "{lines:?}");
    assert!(theirs.handle.disconnect_requested());
    assert!(!meta.outbound_failed_too_many_times(&pro()));
}

/// Rules out "is not connected to us" (active/dkgsession.cpp:293-297).
// Core: evo/mnauth.cpp:19-49, :158-176; active/dkgsession.cpp:274-278
#[test]
fn a7_a_masternode_connection_is_verified_as_our_protx() {
    for link in [
        Link::Inbound {
            masternode: true,
            probe: false,
        },
        Link::Outbound {
            masternode: true,
            probe: false,
        },
    ] {
        let meta = Arc::new(MasternodeMetaMan::new());
        let mut out = None;
        let lines = lines_of(|| out = Some(linked(link, &meta)));
        let (_us, _ours, _judge, theirs, sent) = out.unwrap();
        assert_eq!(sent_mnauth(&sent), PRO, "{link:?}");
        let valid = format!(
            "CMNAuth::ProcessMessage -- Valid MNAUTH for {}, peer={}",
            pro(),
            theirs.handle.id()
        );
        assert!(lines.contains(&valid), "{link:?}: {lines:?}");
        assert!(theirs.handle.masternode_connection(), "{link:?}");
        assert!(!theirs.handle.disconnect_requested(), "{link:?}");
        assert_eq!(theirs.handle.verified_proreg_hash(), PRO, "{link:?}");
        assert_eq!(theirs.handle.verified_pubkey_hash(), operator().pubkey_hash, "{link:?}");
    }
}

/// Both sides send MNAUTH, as two Core DKG members do. Each is left verified as the other's
/// proTx, and rdashd does not disconnect the judge.
// Core: evo/mnauth.cpp:19-49, :116-176 at v24.0.0
#[test]
fn a7_both_sides_verify_the_others_protx_and_rdashd_keeps_the_judge() {
    let judge_key = key_ref(&seated(JUDGE, &judge_secret()));
    for link in MASTERNODE_LINKS {
        let meta = Arc::new(MasternodeMetaMan::new());
        let (_us, ours, _judge, theirs, sent) = linked(link, &meta);
        assert_eq!(sent_mnauth(&sent), PRO, "{link:?}");
        assert_eq!(theirs.handle.verified_proreg_hash(), PRO, "{link:?}");
        assert_eq!(ours.handle.verified_proreg_hash(), JUDGE, "{link:?}");
        assert_eq!(ours.handle.verified_pubkey_hash(), judge_key.pubkey_hash, "{link:?}");
        assert!(!ours.handle.disconnect_requested(), "{link:?}");
        assert!(!theirs.handle.disconnect_requested(), "{link:?}");
    }
}

/// The `m_masternode_connection` byte rdashd sends in `version`: the inbound peer's
/// `fOtherMasternode`, copied before `PushNodeVersion`, or the flag the outbound link was
/// opened with.
// Core: net_processing.cpp:1586, :4012-4013 at v24.0.0
#[test]
fn a7_version_sends_the_masternode_connection_flag() {
    for link in LINKS {
        let meta = Arc::new(MasternodeMetaMan::new());
        let (_us, _ours, _judge, _theirs, sent) = linked(link, &meta);
        let flag = masternode_flag_byte(version_payload(&sent));
        assert_eq!(flag, u8::from(link.masternode()), "{link:?}");
    }
}

mod maintenance {
    use std::collections::HashSet;
    use std::time::Duration;

    use dash_p2p::{Address, Attach, MemSocket, PeerHandle};
    use dash_params::LlmqType;
    use dashd_net::sim::DialSim;

    use super::*;
    use crate::mnsync::MnMaintenance;
    use crate::shutdown::ShutdownState;

    /// A node at its outbound count (one full-relay slot, filled by a plain outbound peer).
    fn at_count() -> (DialSim, Vec<(PeerHandle, MemSocket)>) {
        let sim = DialSim::new(regtest(), |cfg| {
            cfg.dns_seed = false;
            cfg.fixed_seeds = false;
            cfg.max_outbound_full_relay = 1;
            cfg.max_outbound_block_relay = 0;
            cfg.max_feeler = 0;
        });
        let plain = attach(&sim, 1, ConnectionType::OutboundFullRelay, false);
        (sim, vec![plain])
    }

    /// A connection from or to `31.9.0.<host>`; the far end is kept open.
    fn attach(
        sim: &DialSim,
        host: u8,
        conn_type: ConnectionType,
        masternode: bool,
    ) -> (PeerHandle, MemSocket) {
        let (ours, theirs) = MemSocket::pair();
        let peer = sim.core.attach(Attach {
            sock: Arc::new(ours),
            addr: Address {
                service: Service::new(NetAddr::ipv4([31, 9, 0, host]), 9999),
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
            masternode,
            masternode_probe: false,
        });
        (peer, theirs)
    }

    /// The node's minute task, synced, over `quorum_nodes`: whether it closed `peer`.
    fn closes(sim: &DialSim, quorum_nodes: &Arc<QuorumNodes>, peer: &PeerHandle) -> bool {
        let sync = Arc::new(MasternodeSync::new(VirtualClock::at(SIM_START), Box::new(Quiet)));
        sync.switch_to_next_asset();
        MnMaintenance::new(
            Arc::clone(&sim.core),
            sync,
            Arc::clone(quorum_nodes),
            Arc::new(ShutdownState::new()),
            Arc::new(Logger::new()),
        )
        .tick();
        peer.disconnect_requested()
    }

    fn verify(sim: &DialSim, peer: &PeerHandle, pro: [u8; 32]) {
        assert!(sim.core.try_mark_verified(peer.id(), pro, [1; 32]));
    }

    /// Rules out "is not connected to us" (active/dkgsession.cpp:293-297).
    // Core: masternode/utils.cpp:45-61 at v24.0.0
    #[test]
    fn a7_maintenance_keeps_a_verified_inbound_masternode_connection() {
        let (sim, _keep) = at_count();
        let (peer, _far) = attach(&sim, 2, ConnectionType::Inbound, true);
        verify(&sim, &peer, PRO);
        sim.clock.advance(Duration::from_secs(3600));
        assert!(!closes(&sim, &Arc::new(QuorumNodes::default()), &peer));
    }

    /// Rules out "is not connected to us" (active/dkgsession.cpp:293-297).
    // Core: masternode/utils.cpp:45-57, :74-81 at v24.0.0
    #[test]
    fn a7_maintenance_keeps_a_verified_outbound_quorum_connection_only() {
        for (member, relay, kept) in
            [(true, false, true), (false, true, true), (false, false, false)]
        {
            let (sim, _keep) = at_count();
            let (peer, _far) = attach(&sim, 2, ConnectionType::OutboundFullRelay, true);
            verify(&sim, &peer, PRO);
            let quorum_nodes = Arc::new(QuorumNodes::default());
            if member {
                quorum_nodes.set_nodes(LlmqType::LlmqTest, [9; 32], HashSet::from([PRO]));
            }
            if relay {
                quorum_nodes.set_relay_members(
                    LlmqType::LlmqTest,
                    [9; 32],
                    HashSet::from([PRO]),
                    &[],
                );
            }
            sim.clock.advance(Duration::from_secs(3600));
            assert_eq!(closes(&sim, &quorum_nodes, &peer), !kept, "{member} {relay}");
        }
    }

    /// Rules out "failed to connect to it too many times" (active/dkgsession.cpp:310-313).
    // Core: net.h:70; masternode/utils.cpp:42-44, :62-64 at v24.0.0
    #[test]
    fn a7_maintenance_gives_probes_and_unverified_masternodes_five_seconds() {
        for probe in [false, true] {
            let (sim, _keep) = at_count();
            let (peer, _far) = attach(&sim, 2, ConnectionType::Inbound, true);
            if probe {
                verify(&sim, &peer, PRO);
                peer.set_masternode_probe(true);
            }
            let quorum_nodes = Arc::new(QuorumNodes::default());
            sim.clock.advance(Duration::from_secs(4));
            assert!(!closes(&sim, &quorum_nodes, &peer), "probe={probe} at 4 s");
            sim.clock.advance(Duration::from_secs(1));
            assert!(closes(&sim, &quorum_nodes, &peer), "probe={probe} at 5 s");
        }
    }
}

const MASTERNODE_LINKS: [Link; 2] = [
    Link::Inbound {
        masternode: true,
        probe: false,
    },
    Link::Outbound {
        masternode: true,
        probe: false,
    },
];

/// Rules out "does not serve compact block filters" (active/dkgsession.cpp:306-308).
// Core: init.cpp:1101-1103; net_processing.cpp:1582-1586, :4069 at v24.0.0
#[test]
fn a7_masternode_connections_advertise_compact_filters() {
    for link in MASTERNODE_LINKS {
        let meta = Arc::new(MasternodeMetaMan::new());
        let (_us, _ours, _judge, theirs, _sent) = linked(link, &meta);
        assert_eq!(theirs.handle.verified_proreg_hash(), PRO, "{link:?}");
        assert!(theirs.handle.masternode_connection(), "{link:?}");
        assert!(
            theirs.handle.version_services().has(ServiceFlags::NODE_COMPACT_FILTERS),
            "{link:?}"
        );
    }
}

/// `NODE_COMPACT_FILTERS` follows `-peerblockfilters` alone, the index not yet open.
// Core: init.cpp:1242-1249 at v24.0.0
#[test]
fn a7_compact_filters_follow_peerblockfilters() {
    for (argv, on) in [
        (&["-blockfilterindex=basic", "-peerblockfilters=1"][..], true),
        (&["-blockfilterindex=basic", "-peerblockfilters=0"], false),
        (&["-blockfilterindex=basic"], false),
        (&[], false),
    ] {
        let args = test_args(argv);
        let net = parameter_interaction(&Ui::new(Arc::new(Logger::new())), &args).unwrap();
        let role = MasternodeRole::from_args(&args);
        let (cfg, _) = net_config(&args, &regtest(), &net, &role).unwrap();
        assert_eq!(cfg.local_services.has(ServiceFlags::NODE_COMPACT_FILTERS), on, "{argv:?}");
    }
}

/// Rules out "does not relay transactions" (active/dkgsession.cpp:302-305).
// Core: init.cpp:1404-1405; net_processing.cpp:1582-1586, :4070 at v24.0.0
#[test]
fn a7_masternode_connections_relay_transactions() {
    for link in MASTERNODE_LINKS {
        let meta = Arc::new(MasternodeMetaMan::new());
        let (_us, _ours, _judge, theirs, _sent) = linked(link, &meta);
        assert_eq!(theirs.handle.verified_proreg_hash(), PRO, "{link:?}");
        assert!(theirs.handle.masternode_connection(), "{link:?}");
        assert!(theirs.handle.version_relay_txs(), "{link:?}");
    }
}
