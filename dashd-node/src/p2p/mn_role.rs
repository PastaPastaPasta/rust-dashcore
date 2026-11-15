//! BL-703a: the masternode role step 6 reads, the network core's `m_active_masternode` it sets,
//! and step 12's accept refusal over the real `mn_sync`. The sims are built from the node's own
//! products ([`net_config`], [`refuse_unsynced`]); W3 runs the node's init on loopback. The
//! signer and its 7c install are AM1's (BL-703b), so the active sims install a stand-in signer.

use std::io::{Read, Write};
use std::net::TcpStream;

use dash_args::SettingsValue;
use dashd_masternode::sync::NodeSyncNotifier;
use dashd_p2p::sim::masternode::FakeSigner;
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, Sim, SimPeer, VirtualClock, v1_frame};
use tempfile::tempdir;

use super::amb::{net_setup, node, regtest};
use super::*;
use crate::init::test_args;
use crate::test_log::logged;

/// Core's line for an inbound connection an unsynced masternode skips.
// Core: net.cpp:1980
const SKIPPED: &str =
    "AcceptConnection -- blockchain is not synced yet, skipping inbound connection attempt";

const KEY: &str = "-masternodeblsprivkey=00";

fn role(argv: &[&str]) -> MasternodeRole {
    MasternodeRole::from_args(&test_args(argv))
}

struct Quiet;

impl NodeSyncNotifier for Quiet {
    fn sync_reset(&self) {}

    fn sync_finished(&self) {}
}

/// A fresh `mn_sync`: at BLOCKCHAIN.
fn mn_sync() -> Arc<MasternodeSync> {
    Arc::new(MasternodeSync::new(VirtualClock::at(SIM_START), Box::new(Quiet)))
}

/// A sim over the node's products for `argv`: the core's config from [`net_config`], the
/// identity from its options, the accept refusal from [`refuse_unsynced`] over `sync`, and `mn`.
fn sim(
    argv: &[&str],
    max_connections: i32,
    sync: &Arc<MasternodeSync>,
    mn: MasternodeSeams,
) -> Sim {
    let args = test_args(argv);
    let role = MasternodeRole::from_args(&args);
    let (cfg, ident) = net_config(&args, &regtest(), &net_setup(max_connections), &role).unwrap();
    assert_eq!(cfg.active_masternode, role.active());
    let identity = local_identity(&cfg.params, &ident).unwrap();
    Sim::with_parts(cfg, identity, refuse_unsynced(&role, sync), mn)
}

/// The seams of an active masternode: AM1's signer, here a stand-in.
fn signer() -> MasternodeSeams {
    MasternodeSeams {
        signer: Some(Arc::new(FakeSigner::new(None))),
        ..MasternodeSeams::default()
    }
}

fn candidates(sim: &Sim) -> Vec<i64> {
    sim.core.eviction_candidates().iter().map(|c| c.id).collect()
}

fn inbound(sim: &Sim, i: u8) -> Option<SimPeer> {
    sim.accept_from(i, NetAddr::ipv4([20 + i, 1, 1, 1]), Permissions::NONE)
}

/// W1: the role is Core's 7c predicate, a non-empty `-masternodeblsprivkey` (not `IsArgSet`),
/// and the core's `m_active_masternode` is the role's. A `settings.json` array or object is set
/// and not the empty string: active, with no error at step 6.
// Core: init.cpp:2156, :2616
#[test]
fn w1_role_is_a_non_empty_key_and_sets_the_cores_flag() {
    assert!(!role(&[]).active());
    assert!(!role(&["-masternodeblsprivkey="]).active());
    assert!(role(&[KEY]).active());
    // `GetArg` gives "0" for a negated key.
    assert!(role(&["-nomasternodeblsprivkey"]).active());
    for value in [SettingsValue::Arr(Vec::new()), SettingsValue::Obj(Vec::new())] {
        let args = test_args(&[]);
        args.lock_settings(|s| s.rw_settings.insert("masternodeblsprivkey".into(), value.clone()));
        assert!(MasternodeRole::from_args(&args).active(), "{value:?}");
    }
    for argv in [&[][..], &[KEY]] {
        let args = test_args(argv);
        let role = MasternodeRole::from_args(&args);
        let (cfg, _) = net_config(&args, &regtest(), &net_setup(125), &role).unwrap();
        assert_eq!(cfg.active_masternode, role.active(), "{argv:?}");
    }
}

/// W2.1: an active masternode skips inbound connections with Core's line while `mn_sync` is at
/// BLOCKCHAIN, and admits the next one once it is past: the refusal reads the sync live.
// Core: net.cpp:1979-1982
#[test]
fn w2_1_active_unsynced_skips_inbound_until_the_blockchain_is_synced() {
    let sync = mn_sync();
    let sim = sim(&[KEY], 125, &sync, signer());
    let (peer, lines) = logged(|| inbound(&sim, 1));
    assert!(peer.is_none());
    assert!(lines.iter().any(|(_, l)| l == SKIPPED), "{lines:?}");
    assert!(sim.core.peers().is_empty());
    sync.switch_to_next_asset();
    assert!(inbound(&sim, 2).is_some());
}

/// W2.2: at the inbound limit, an unsynced active masternode still evicts one inbound peer, then
/// drops the newcomer (Core refuses after the eviction loop).
// Core: net.cpp:1963-1982
#[test]
fn w2_2_active_unsynced_at_the_limit_evicts_then_drops() {
    let sync = mn_sync();
    sync.switch_to_next_asset();
    // 40 inbound slots: 51 connections less 8 + 2 + 1 outbound.
    let sim = sim(&[KEY], 51, &sync, signer());
    let held: Vec<_> = (0..40).map(|i| inbound(&sim, i).expect("a free slot")).collect();
    // Past the fresh peers' first second, so each is a candidate.
    sim.clock.set_mock_time(SIM_START + 1);
    sync.reset(true, false);
    assert!(inbound(&sim, 40).is_none());
    assert_eq!(held.iter().filter(|p| p.handle.disconnect_requested()).count(), 1);
    assert_eq!(sim.core.peers().len(), 40);
}

/// W2.3: a node that is not a masternode never refuses, synced or not.
#[test]
fn w2_3_inactive_unsynced_admits() {
    let sync = mn_sync();
    assert!(!refuse_unsynced(&role(&[]), &sync)());
    let sim = sim(&[], 125, &sync, MasternodeSeams::default());
    assert!(inbound(&sim, 1).is_some());
}

/// W2.4: on an active masternode a fresh inbound peer is not an eviction candidate until its
/// first message after the handshake is not `mnauth`, and a peer verified through
/// [`NetCore::try_mark_verified`](dash_p2p::NetCore::try_mark_verified) never is, even after a
/// first message that is not `mnauth`. On any other node every peer is one.
// Core: net.cpp:1802-1818, :4514-4539
#[test]
fn w2_4_fresh_inbound_protection_follows_the_role() {
    for (argv, mn) in [(&[KEY][..], signer()), (&[], MasternodeSeams::default())] {
        let sync = mn_sync();
        sync.switch_to_next_asset();
        let mut sim = sim(argv, 125, &sync, mn);
        let quiet = inbound(&sim, 1).unwrap();
        let chatty = inbound(&sim, 2).unwrap();
        let verified = inbound(&sim, 3).unwrap();
        for p in [&chatty, &verified] {
            sim.handshake(p, PeerVersionMsg::v24(SIM_START));
            sim.sent(p);
            sim.deliver(p, &[("ping", 7u64.to_le_bytes().to_vec())]);
        }
        sim.pass();
        for p in [&chatty, &verified] {
            assert_ne!(p.handle.first_message_time(), 0);
            assert!(!p.handle.first_message_is_mnauth());
        }
        assert!(sim.core.try_mark_verified(verified.handle.id(), [7; 32], [8; 32]));
        let (quiet, chatty, verified) =
            (quiet.handle.id(), chatty.handle.id(), verified.handle.id());
        let active = !argv.is_empty();
        let want = if active {
            vec![chatty]
        } else {
            vec![quiet, chatty, verified]
        };
        assert_eq!(candidates(&sim), want, "{argv:?}");
    }
}

/// BL-704 test 1 at the node: the fourth inbound leg of one protx is refused with Core's line,
/// and the stats `getpeerinfo` reads keep both its hashes zero, so it prints neither
/// `verified_*` field (dashd-rpc peers.rs), while each granted leg carries both.
// Core: net.cpp:4531-4534; rpc/net.cpp:239-244
#[test]
fn the_fourth_verified_leg_is_refused_and_unlisted_as_verified() {
    let sync = mn_sync();
    sync.switch_to_next_asset();
    let sim = sim(&[KEY], 125, &sync, signer());
    let legs: Vec<_> = (1..=4).map(|i| inbound(&sim, i).unwrap()).collect();
    // Asymmetric, so the log line's reversed byte order is checked.
    let proreg: [u8; 32] = core::array::from_fn(|i| i as u8);
    let pubkey = [8; 32];
    for leg in &legs[..3] {
        assert!(sim.core.try_mark_verified(leg.handle.id(), proreg, pubkey));
    }
    let last = legs[3].handle.id();
    let (granted, lines) = logged(|| sim.core.try_mark_verified(last, proreg, pubkey));
    assert!(!granted);
    let line = format!(
        "CConnman::TryMarkVerified -- refusing to verify masternode {}: already 3 verified inbound connections, peer={last}",
        proreg.iter().rev().map(|b| format!("{b:02x}")).collect::<String>()
    );
    assert!(lines.iter().any(|(_, l)| *l == line), "{lines:?}");
    for leg in &legs {
        let stats = dashd_net::NodeStats::of(&sim.core, &leg.handle);
        let want = if leg.handle.id() == last {
            ([0; 32], [0; 32])
        } else {
            (proreg, pubkey)
        };
        assert_eq!((stats.verified_proreg_hash, stats.verified_pubkey_hash), want);
    }
}

/// W2.5: the processor's masternode flag follows the signer. An inbound peer that says it is a
/// masternode connection is kept by a masternode and disconnected by any other node.
// Core: net_processing.cpp:4010-4018
#[test]
fn w2_5_an_inbound_masternode_connection_needs_a_signer() {
    for (argv, mn) in [(&[KEY][..], signer()), (&[], MasternodeSeams::default())] {
        let sync = mn_sync();
        sync.switch_to_next_asset();
        let mut sim = sim(argv, 125, &sync, mn);
        let active = !argv.is_empty();
        assert_eq!(sim.shared.identity().masternode, active);
        assert_eq!(sim.core.config().active_masternode, active);
        let p = inbound(&sim, 1).unwrap();
        let version = PeerVersionMsg {
            masternode: true,
            ..PeerVersionMsg::v24(SIM_START)
        };
        sim.deliver(&p, &[("version", version.payload())]);
        sim.pass();
        assert_eq!(p.handle.disconnect_requested(), !active, "{argv:?}");
    }
}

/// A port no listener holds now.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// The command of the first V1 message on `s`.
fn first_command(s: &mut TcpStream) -> String {
    let mut header = [0; 24];
    s.read_exact(&mut header).expect("a message");
    String::from_utf8_lossy(&header[4..16]).trim_end_matches('\0').to_owned()
}

/// W3, a node that is not a masternode: the node's init (step 6's role and `Net::new`, the
/// checks, step 7c's `attach`, step 12's `start`) leaves the core's flag and the processor's
/// false, and its listener answers an inbound `version` while `mn_sync` is at BLOCKCHAIN. The
/// active role's half is BL-703b's.
#[test]
fn w3_inactive_node_init_sets_no_flag_and_refuses_no_inbound() {
    let dir = tempdir().unwrap();
    let bind = format!("-bind=127.0.0.1:{}", free_port());
    let mut node = node(dir.path(), &[&bind, "-listenonion=0", "-dnsseed=0", "-fixedseeds=0"]);
    let net = node.net().expect("step 6 built it");
    assert!(!net.role().active());
    assert!(!net.core().config().active_masternode);
    assert!(!net.shared().expect("attached").identity().masternode);
    let at = net.attached.as_ref().expect("attached");
    assert!(!at.sync.as_ref().expect("the node's mn_sync").is_blockchain_synced());
    let addr = net.connman().expect("started").listen_addrs()[0];
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(realv::PATIENCE)).unwrap();
    let version = PeerVersionMsg::v24(SIM_START).payload();
    s.write_all(&v1_frame(&regtest(), "version", &version)).unwrap();
    assert_eq!(first_command(&mut s), "version");
    node.shutdown();
}

/// BL-705: Core starts `mncon` only with `m_masternode_connections` (init.cpp:2617-2619,
/// net.cpp:4118-4122). rdashd's one term is the role, so the default node runs `dnsseed`,
/// `addcon` and `opencon` and no `mncon`; an active role would start it.
#[test]
fn bl705_the_default_node_starts_no_mncon_thread() {
    assert!(!masternode_connections(&MasternodeRole::default()));
    assert!(masternode_connections(&MasternodeRole::from_args(&test_args(&[KEY]))));
    let dir = tempdir().unwrap();
    let bind = format!("-bind=127.0.0.1:{}", free_port());
    let mut node = node(dir.path(), &[&bind, "-listenonion=0", "-dnsseed=0", "-fixedseeds=0"]);
    let net = node.net().expect("step 6 built it");
    assert!(!net.role().active());
    assert_eq!(net.connman().expect("started").thread_names(), ["dnsseed", "addcon", "opencon"]);
    node.shutdown();
}

/// BL-705: Core's masternode loop returns when `GetArgs("-connect")` has a value (net.cpp:3418):
/// `-connect=<addr>` and `-connect=0` stop it, a bare `-noconnect` does not, though both turn off
/// automatic outbound.
#[test]
fn bl705_only_a_listed_connect_stops_the_masternode_loop() {
    for (argv, listed, connect) in [
        (&[][..], false, None),
        (&["-noconnect"][..], false, Some(vec![])),
        (&["-connect=0"][..], true, Some(vec![])),
        (&["-connect=1.2.3.4"][..], true, Some(vec!["1.2.3.4".to_owned()])),
        (&["-noconnect", "-connect=1.2.3.4"][..], true, Some(vec!["1.2.3.4".to_owned()])),
    ] {
        let args = test_args(argv);
        let (cfg, _) =
            net_config(&args, &regtest(), &net_setup(125), &MasternodeRole::default()).unwrap();
        assert_eq!((cfg.connect_listed, cfg.connect), (listed, connect), "{argv:?}");
    }
}

/// The two masternode flags must agree at step 7c: an active role with no signer installed (AM1
/// is not there yet) stops `attach`.
#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "the signer is installed exactly when the role is active")]
fn attach_requires_the_signer_and_the_role_to_agree() {
    let dir = tempdir().unwrap();
    let mut node = node(dir.path(), &[]);
    node.stop_net();
    let args = test_args(&[KEY]);
    let role = MasternodeRole::from_args(&args);
    let clock: Arc<dyn Clock> = Arc::clone(&node.clock) as _;
    let net_dir = dir.path().join("active");
    std::fs::create_dir_all(&net_dir).unwrap();
    let mut net =
        Net::new(&args, &regtest(), &net_setup(125), role, Arc::clone(&clock), &net_dir).unwrap();
    net.check_options(&args, &crate::logging::Logger::new()).unwrap();
    net.attach_with(node.chain().expect("V runs"), None, clock, None, |access| access);
}
