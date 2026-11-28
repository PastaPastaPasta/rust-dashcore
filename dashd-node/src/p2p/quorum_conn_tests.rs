//! P7 W3: [`NodeQuorumConnections::check`] over the node's registry and a sim's network core,
//! with the chain's scan and ancestors from a fixed read, and once over a regtest node's V.
// Core: llmq/net_quorum.cpp:408-497, :766-835; net.cpp:4380-4463 at v24.0.2

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use dash_clock::Clock;
use dash_consensus::BlsCtx;
use dash_dml::MnList;
use dash_evo::spork::SporkId;
use dash_evo::types::ProTxHash;
use dash_netaddr::{NetAddr, Service};
use dash_p2p::{CommandString, ConnectionType, Permissions, Why};
use dash_params::{ChainParamsBuilder, LlmqParams, LlmqType, Network, Release};
use dash_primitives::{AddrV1, BlockHash, NetInfo, NetInfoV1, ServiceV1};
use dash_types::Numeric;
use dashd_masternode::meta::MasternodeMetaMan;
use dashd_masternode::mnconn::{LlmqTopology, QuorumInfo, QuorumTopology};
use dashd_masternode::tip::{TipListRef, TipSource};
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, Sim, SimPeer};
use dashd_p2p::{ProcessorOptions, Registry, Seams};
use dashd_quorums::Role;
use tempfile::tempdir;

use super::amb::node;
use super::*;
use crate::init::test_args;
use crate::test_log::logged;

const SEED: [u8; 32] = [5; 32];
/// Core's functional-test spork key (test_framework.py:1519).
const SPORK_KEY: &str = "-sporkkey=cP4EKFyJsHT39LDqgdcB43Y3YXjNyjb5Fuas1GQSeAtjnZWmZEQK";
const TEST: LlmqType = LlmqType::LlmqTest;
const QSENDRECSIGS: (CommandString, &[u8]) = (CommandString::QSENDRECSIGS, &[1]);

fn pro(n: u8) -> [u8; 32] {
    [n; 32]
}

/// The block hash the fixed read gives at `height`.
fn at(height: i32) -> [u8; 32] {
    let mut hash = [0xa0; 32];
    hash[..4].copy_from_slice(&height.to_le_bytes());
    hash
}

fn regtest() -> Arc<ChainSetup> {
    Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap())
}

fn params(llmq_type: LlmqType) -> LlmqParams {
    *regtest().consensus.llmq(llmq_type).expect("regtest runs the type")
}

/// A scan and ancestors fixed for every tip, and the scans asked for.
#[derive(Default)]
struct FixedRead {
    scan: QuorumScan,
    asked: Mutex<Vec<(LlmqType, BlockHash, usize)>>,
}

impl QuorumScanRead for FixedRead {
    fn scan(&self, llmq_type: LlmqType, tip: &BlockHash, count: usize) -> Option<QuorumScan> {
        self.asked.lock().unwrap().push((llmq_type, *tip, count));
        Some(self.scan.clone())
    }

    fn ancestor(&self, _: &BlockHash, height: i32) -> Option<[u8; 32]> {
        Some(at(height))
    }

    fn members(&self, llmq_type: LlmqType, base: &BlockHash) -> Option<Vec<[u8; 32]>> {
        let q = self.scan.quorums.iter().find(|q| q.base_hash == *base.as_bytes())?;
        (q.llmq_type == llmq_type).then(|| q.members.clone())
    }
}

fn quorum(llmq_type: LlmqType, base_height: i32, members: &[[u8; 32]]) -> QuorumInfo {
    QuorumInfo {
        llmq_type,
        quorum_hash: at(base_height),
        base_hash: at(base_height),
        base_height,
        members: members.to_vec(),
    }
}

fn tip(height: i32) -> TipRef {
    TipRef {
        height,
        hash: BlockHash::from(at(height)),
    }
}

fn masternode(me: [u8; 32], watching: bool) -> Role {
    Role::Masternode {
        pro_tx: Some(ProTxHash::from(me)),
        watching,
    }
}

/// A sim's network core, the node's registry and spork manager, with spork 21 at `spork21`.
struct Fixture {
    sim: Sim,
    registry: Arc<QuorumNodes>,
    read: Arc<FixedRead>,
    sporks: Arc<SporkManager>,
    metaman: Arc<MasternodeMetaMan>,
    _dir: tempfile::TempDir,
    checker: NodeQuorumConnections,
}

fn fixture(spork21: Option<i64>, scan: QuorumScan) -> Fixture {
    fixture_meta(spork21, scan, true)
}

fn empty_tip() -> TipMnList {
    TipMnList::fixed(tip_list_ref())
}

/// [`fixture`] with a metaman whose cache loaded (`valid`) or not.
fn fixture_meta(spork21: Option<i64>, scan: QuorumScan, valid: bool) -> Fixture {
    fixture_with(spork21, scan, valid, empty_tip())
}

/// [`fixture_meta`] over `tip_list`.
fn fixture_with(
    spork21: Option<i64>,
    scan: QuorumScan,
    valid: bool,
    tip_list: TipMnList,
) -> Fixture {
    let p = regtest();
    let id = local_identity(&p, &IdentityOptions::new()).unwrap();
    let sim = Sim::with(
        Arc::clone(&p),
        id,
        ProcessorOptions::default(),
        Registry::new(),
        Seams::default(),
        |_| {},
    );
    let clock: Arc<dyn Clock> = Arc::clone(&sim.clock) as _;
    let sporks =
        crate::sporks::spork_manager(&test_args(&[SPORK_KEY]), &p, Arc::clone(&clock)).unwrap();
    if let Some(value) = spork21 {
        sporks.update_spork(SporkId::SPORK_21_QUORUM_ALL_CONNECTED, value).unwrap();
    }
    let registry = Arc::new(QuorumNodes::default());
    let read = Arc::new(FixedRead {
        scan,
        ..FixedRead::default()
    });
    let dir = tempdir().unwrap();
    let metaman = Arc::new(MasternodeMetaMan::new());
    if valid {
        assert!(metaman.load_cache(&dir.path().join("mncache.dat"), [1, 2, 3, 4], false));
    }
    assert_eq!(metaman.is_valid(), valid);
    let checker = NodeQuorumConnections::new(QuorumConnParts {
        params: p,
        chain: Arc::clone(&read) as _,
        registry: Arc::clone(&registry),
        core: Arc::clone(&sim.core),
        sporks: Arc::clone(&sporks),
        metaman: Arc::clone(&metaman),
        tip_list,
        clock,
        watch_seed: SEED,
    });
    Fixture {
        sim,
        registry,
        read,
        sporks,
        metaman,
        _dir: dir,
        checker,
    }
}

impl Fixture {
    /// An inbound peer at 10.0.0.`n` past the handshake, verified as `pro`.
    fn verified(&mut self, n: u8, pro: [u8; 32]) -> SimPeer {
        let peer = self.sim.connect(n, ConnectionType::Inbound, Permissions::NONE);
        self.sim.handshake(&peer, PeerVersionMsg::v24(SIM_START));
        while self.sim.pass() {}
        assert!(peer.handle.successfully_connected());
        assert!(self.sim.core.try_mark_verified(peer.handle.id(), pro, [8; 32]));
        self.sim.sent(&peer);
        peer
    }

    /// [`QuorumConnectionsCheck::check`] for `llmq_type` with the chain's parameters for it.
    fn check(&self, llmq_type: LlmqType, tip: &TipRef, role: &Role) {
        QuorumConnectionsCheck::check(&self.checker, llmq_type, &params(llmq_type), tip, role);
    }

    fn qsendrecsigs(&mut self, peer: &SimPeer) -> bool {
        let sent = self.sim.sent(peer);
        sent.iter().any(|(c, p)| (*c, p.as_slice()) == QSENDRECSIGS)
    }

    /// The one registered quorum's node set.
    fn nodes(&self) -> HashSet<[u8; 32]> {
        let groups = self.registry.quorum_groups();
        assert_eq!(groups.len(), 1, "one quorum registered");
        groups[0].iter().copied().collect()
    }

    /// The members of `among` in the union of the relay sets.
    fn relay(&self, among: &[[u8; 32]]) -> HashSet<[u8; 32]> {
        among.iter().copied().filter(|m| self.registry.is_relay_member(m)).collect()
    }
}

fn members(n: u8) -> Vec<[u8; 32]> {
    (1..=n).map(pro).collect()
}

/// `Role::None` changes nothing and reads no chain, where an observer at the same tip prunes the
/// registry to the current DKG cycle's quorum.
// Core: llmq/net_quorum.cpp:379, :392, :427-431, :492-496
#[test]
fn w3_role_none_changes_nothing_where_an_observer_prunes() {
    for (role, kept) in [(Role::None, false), (Role::Observer, true)] {
        let f = fixture(None, QuorumScan::default());
        f.registry.set_nodes(TEST, at(24), HashSet::from([pro(1)]));
        f.registry.set_nodes(TEST, [9; 32], HashSet::from([pro(2)]));
        f.check(TEST, &tip(30), &role);
        let want = if kept {
            HashSet::from([at(24)])
        } else {
            HashSet::from([at(24), [9; 32]])
        };
        assert_eq!(f.registry.quorums(TEST), want, "{role:?}");
        assert_eq!(f.read.asked.lock().unwrap().len(), usize::from(kept), "{role:?}");
    }
}

/// Spork 21 on (mesh): a member masternode registers Core's outbound mesh as its nodes and every
/// other member as relay members, and a verified relay member's peer gets QSENDRECSIGS once, a
/// disconnecting one none; an observer registers its one watch connection as both sets.
// Core: llmq/net_quorum.cpp:791-811, :829-833; llmq/utils.cpp:753-775, :832-845; net.cpp:4400-4409;
// net.cpp:4875-4878
#[test]
fn w3_mesh_member_and_observer_sets() {
    let members = members(6);
    let me = pro(3);
    let scan = QuorumScan {
        quorums: vec![quorum(TEST, 24, &members)],
        rotation_enabled: false,
    };
    let topology = LlmqTopology;

    let mut f = fixture(Some(0), scan.clone());
    let relay_peer = f.verified(1, pro(5));
    let outsider = f.verified(2, pro(9));
    let leaving = f.verified(3, pro(6));
    f.sim.core.disconnect(leaving.handle.id(), Why::Processor("leaving"));
    f.check(TEST, &tip(30), &masternode(me, false));
    assert_eq!(f.nodes(), topology.quorum_connections(&params(TEST), &members, me, true, true));
    let others: HashSet<_> = members.iter().copied().filter(|m| *m != me).collect();
    assert_eq!(f.relay(&members), others);
    assert!(f.qsendrecsigs(&relay_peer));
    assert!(relay_peer.handle.masternode_iqr());
    assert!(!f.qsendrecsigs(&outsider));
    assert!(!outsider.handle.masternode_iqr());
    assert!(!leaving.handle.masternode_iqr(), "a disconnecting peer is not fully connected");
    f.check(TEST, &tip(31), &masternode(me, false));
    assert!(!f.qsendrecsigs(&relay_peer), "QSENDRECSIGS goes once");

    let f = fixture(Some(0), scan);
    f.check(TEST, &tip(30), &Role::Observer);
    let watched: HashSet<_> = topology
        .watch_connections(&SEED, TEST, at(24), members.len(), 1)
        .into_iter()
        .map(|i| members[i])
        .collect();
    assert_eq!(watched.len(), 1);
    assert_eq!(f.nodes(), watched);
    assert_eq!(f.relay(&members), watched);
}

/// Spork 21 off (ring): a member's relay members are Core's ring relay set, not every other
/// member, and its nodes are the outbound ring connections.
// Core: llmq/net_quorum.cpp:791-805; llmq/utils.cpp:753-830
#[test]
fn w3_ring_member_relays_to_the_ring() {
    let members = members(12);
    let me = pro(4);
    let topology = LlmqTopology;
    let ring = topology.quorum_relay_members(&members, me, true);
    assert!(!ring.is_empty() && ring.len() < members.len() - 1);
    let f = fixture(
        None,
        QuorumScan {
            quorums: vec![quorum(TEST, 24, &members)],
            rotation_enabled: false,
        },
    );
    f.check(TEST, &tip(30), &masternode(me, false));
    assert_eq!(f.nodes(), topology.quorum_connections(&params(TEST), &members, me, true, false));
    assert_eq!(f.relay(&members), ring);
}

/// Watching: a watching masternode outside a quorum registers its watch connection; a
/// non-watching masternode in one DIP0024 InstantSend quorum adds inter-quorum connections to
/// the other, with Core's lines, and a non-watching outsider of a non-IS type registers nothing.
// Core: llmq/net_quorum.cpp:446-449, :463-488, :806-811
#[test]
fn w3_watch_and_inter_quorum_connections() {
    let topology = LlmqTopology;
    let members = members(5);
    let watch = |base| -> HashSet<[u8; 32]> {
        topology
            .watch_connections(&SEED, TEST, at(base), members.len(), 1)
            .into_iter()
            .map(|i| members[i])
            .collect()
    };
    let scan = QuorumScan {
        quorums: vec![quorum(TEST, 24, &members)],
        rotation_enabled: false,
    };
    let f = fixture(None, scan.clone());
    f.check(TEST, &tip(30), &masternode(pro(77), true));
    assert_eq!(f.nodes(), watch(24));
    assert_eq!(f.relay(&members), watch(24));

    let f = fixture(None, scan);
    f.check(TEST, &tip(30), &masternode(pro(77), false));
    assert!(f.registry.quorums(TEST).is_empty());

    let is_type = regtest().consensus.llmq_type_dip0024_instant_send;
    let mine = [pro(77), pro(78), pro(79)];
    let other = quorum(is_type, 48, &members);
    let f = fixture(
        None,
        QuorumScan {
            quorums: vec![quorum(is_type, 24, &mine), other],
            rotation_enabled: false,
        },
    );
    let ((), lines) = logged(|| f.check(is_type, &tip(50), &masternode(pro(77), false)));
    let lines: Vec<String> = lines.into_iter().map(|(_, l)| l).collect();
    let watched: HashSet<_> = topology
        .watch_connections(&SEED, is_type, at(48), members.len(), 1)
        .into_iter()
        .map(|i| members[i])
        .collect();
    assert!(f.registry.has(is_type, &at(48)));
    assert_eq!(f.relay(&members), watched);
    let adding = format!(
        "NetQuorum::CheckQuorumConnections -- llmqType[{}] h[50] adding mn inter-quorum connections for quorum: [48:{}]",
        is_type.to_base(),
        BlockHash::from(at(48))
    );
    assert!(lines.contains(&adding), "{lines:?}");
}

/// A rotated type keeps the current cycle's quorums at or below the tip's index when the chain's
/// `IsQuorumRotationEnabled` says so, and only the cycle base otherwise.
// Core: llmq/net_quorum.cpp:411-432; llmq/options.cpp:50-62
#[test]
fn w3_rotation_follows_is_quorum_rotation_enabled() {
    let rotated = regtest().consensus.llmq_type_dip0024_instant_send;
    let p = params(rotated);
    assert!(p.use_rotation && p.signing_active_quorum_count >= 2);
    let base = 2 * i32::from(p.dkg_interval);
    for rotation_enabled in [false, true] {
        let f = fixture(
            None,
            QuorumScan {
                quorums: Vec::new(),
                rotation_enabled,
            },
        );
        for h in [base, base + 1, base + 2, 7] {
            f.registry.set_nodes(rotated, at(h), HashSet::from([pro(1)]));
        }
        f.check(rotated, &tip(base + 1), &Role::Observer);
        let want = if rotation_enabled {
            HashSet::from([at(base), at(base + 1)])
        } else {
            HashSet::from([at(base)])
        };
        assert_eq!(f.registry.quorums(rotated), want, "rotation {rotation_enabled}");
        let asked = f.read.asked.lock().unwrap().clone();
        let tip_hash = BlockHash::from(at(base + 1));
        assert_eq!(asked, [(rotated, tip_hash, usize::from(p.keep_old_connections))]);
    }
}

/// Core applies the relay sets before it removes deletable quorums: a stale quorum's relay member
/// still counts for this pass's QSENDRECSIGS, and the stale quorum is gone afterwards. The push
/// follows the quorum's member block and precedes the removal line.
// Core: llmq/net_quorum.cpp:451-462, :492-496; net.cpp:4389-4409, :4459-4463
#[test]
fn w3_ops_apply_in_core_order() {
    let members = members(4);
    let me = pro(2);
    let stale = [9; 32];
    let mut f = fixture(
        Some(0),
        QuorumScan {
            quorums: vec![quorum(TEST, 24, &members)],
            rotation_enabled: false,
        },
    );
    let held = f.verified(1, pro(60));
    f.registry.set_nodes(TEST, stale, HashSet::from([pro(60)]));
    f.registry.set_relay_members(TEST, stale, HashSet::from([pro(60)]), &[]);
    let ((), lines) = logged(|| f.check(TEST, &tip(30), &masternode(me, false)));
    assert!(f.qsendrecsigs(&held), "sent while the stale relay set was still registered");
    assert!(held.handle.masternode_iqr());
    assert_eq!(f.registry.quorums(TEST), HashSet::from([at(24)]));
    assert!(!f.registry.is_relay_member(&pro(60)));
    let removing = format!(
        "NetQuorum::CheckQuorumConnections -- removing masternodes quorum connections for quorum {}:",
        BlockHash::from(stale)
    );
    let lines: Vec<String> = lines.into_iter().map(|(_, l)| l).collect();
    let position = |prefix: &str| lines.iter().position(|l| l.starts_with(prefix));
    let adding = position("EnsureQuorumConnections -- adding").expect("the member block");
    let sending = position("sending qsendrecsigs").expect("the push");
    assert!(adding < sending, "{lines:?}");
    assert_eq!(lines.last(), Some(&removing), "{lines:?}");
}

/// Regtest's LLMQ_TEST, LLMQ_TEST_INSTANTSEND and LLMQ_TEST_V17 share their parameters; each acts
/// through both traits under its own type, and the registry keys its quorum by that type alone.
// Core: llmq/params.h:51, :146, :172, :198; llmq/net_quorum.cpp:397-398, :766-835;
// llmq/net_dkg.cpp:880-883 at v24.0.0
#[test]
fn w3_dk1_types_sharing_params_act_under_their_own_type() {
    let same = [LlmqType::LlmqTest, LlmqType::LlmqTestInstantsend, LlmqType::LlmqTestV17];
    for t in same {
        assert!(regtest().consensus.llmqs.contains(&t));
        assert_eq!(params(t), params(TEST), "{t:?}");
    }
    let members = members(3);
    let registered = |f: &Fixture| -> Vec<LlmqType> {
        same.into_iter().filter(|other| f.registry.has(*other, &at(24))).collect()
    };
    for t in same {
        let scan = QuorumScan {
            quorums: vec![quorum(t, 24, &members)],
            rotation_enabled: false,
        };
        let f = fixture(Some(0), scan.clone());
        f.check(t, &tip(30), &masternode(pro(1), false));
        assert_eq!(registered(&f), [t], "check as {t:?}");

        let f = fixture(Some(0), scan);
        DkgConnections::on_initialized(
            &f.checker,
            t,
            &params(t),
            &tip(24),
            &ProTxHash::from(pro(1)),
            false,
            false,
        );
        assert_eq!(registered(&f), [t], "on_initialized as {t:?}");
    }
}

/// V's reads at a regtest node's genesis: the scan finds no quorum and no rotation, the ancestor
/// at height 0 is genesis, and an unknown tip reads nothing. An observer's check there keeps the
/// cycle base and removes a stale quorum.
// Core: llmq/net_quorum.cpp:427-431, :444, :492-496; llmq/options.cpp:50-62
#[test]
fn w3_v_reads_at_genesis() {
    let dir = tempdir().unwrap();
    let mut node = node(dir.path(), &["-listen=0", "-dnsseed=0", "-fixedseeds=0"]);
    let chain = node.chain().expect("V runs").handle().clone();
    let view = chain.view_handle().view();
    let genesis = chain.reader().entry(view.snap.active.at(0).unwrap()).unwrap().hash;
    assert_eq!(view.snap.tip.tip_height, 0);
    let read: Arc<dyn QuorumScanRead> = Arc::new(chain);
    assert_eq!(read.members(TEST, &BlockHash::from([3; 32])), None);
    assert_eq!(read.scan(TEST, &genesis, 3), Some(QuorumScan::default()));
    assert_eq!(read.ancestor(&genesis, 0), Some(*genesis.as_bytes()));
    assert_eq!(read.ancestor(&genesis, 1), None);
    let unknown = BlockHash::from([3; 32]);
    assert_eq!(read.scan(TEST, &unknown, 3), None);
    assert_eq!(read.ancestor(&unknown, 0), None);

    let p = regtest();
    let net = node.net().expect("step 6 built it");
    let registry = Arc::new(QuorumNodes::default());
    registry.set_nodes(TEST, *genesis.as_bytes(), HashSet::from([pro(1)]));
    registry.set_nodes(TEST, [9; 32], HashSet::from([pro(2)]));
    let clock: Arc<dyn Clock> = Arc::clone(&node.clock) as _;
    let checker = NodeQuorumConnections::new(QuorumConnParts {
        params: Arc::clone(&p),
        chain: read,
        registry: Arc::clone(&registry),
        core: Arc::clone(net.core()),
        sporks: crate::sporks::spork_manager(&test_args(&[]), &p, Arc::clone(&clock)).unwrap(),
        metaman: Arc::new(MasternodeMetaMan::new()),
        tip_list: TipMnList::fixed(TipListRef {
            list: Arc::new(MnList::default_pre_dip3()),
            bls: BlsCtx::BASIC,
        }),
        clock,
        watch_seed: SEED,
    });
    let at_genesis = TipRef {
        height: 0,
        hash: genesis,
    };
    QuorumConnectionsCheck::check(&checker, TEST, &params(TEST), &at_genesis, &Role::Observer);
    assert_eq!(registry.quorums(TEST), HashSet::from([*genesis.as_bytes()]));
    node.shutdown();
}

fn tip_list_ref() -> TipListRef {
    TipListRef {
        list: Arc::new(MnList::default_pre_dip3()),
        bls: BlsCtx::BASIC,
    }
}

/// A fixed tip list of `list` under the basic scheme.
#[cfg(feature = "dkg-runtime")]
pub(super) fn tip_of(list: MnList) -> TipMnList {
    TipMnList::fixed(TipListRef {
        list: Arc::new(list),
        ..tip_list_ref()
    })
}

fn dip24() -> LlmqType {
    regtest().consensus.llmq_type_dip0024_instant_send
}

fn base_scan(members: &[[u8; 32]]) -> QuorumScan {
    QuorumScan {
        quorums: vec![quorum(dip24(), 24, members)],
        rotation_enabled: false,
    }
}

fn initialized(f: &Fixture, me: [u8; 32], watch: bool, we_are_member: bool) {
    DkgConnections::on_initialized(
        &f.checker,
        dip24(),
        &params(dip24()),
        &tip(24),
        &ProTxHash::from(me),
        watch,
        we_are_member,
    );
}

fn add_probes(f: &Fixture, me: [u8; 32]) {
    initialized(f, me, false, true);
}

fn pending_probes(f: &Fixture) -> HashSet<[u8; 32]> {
    f.sim.core.pending_probes().into_iter().collect()
}

/// DK1 ensure with the members at the quorum's base: a member registers the mesh nodes and every
/// other member as relay members and sends QSENDRECSIGS to a verified relay member; a masternode
/// outside the quorum that does not watch registers nothing.
// Core: llmq/net_dkg.cpp:880-883 at v24.0.0; llmq/net_quorum.cpp:766-835; net.cpp:4389-4410
#[test]
fn dk1_ensure_on_a_member_sets_the_registry() {
    let members = members(5);
    let me = pro(2);
    let mut f = fixture(Some(0), base_scan(&members));
    let relay_peer = f.verified(1, pro(4));
    initialized(&f, me, false, true);
    let mesh = LlmqTopology.quorum_connections(&params(dip24()), &members, me, true, true);
    assert_eq!(f.nodes(), mesh);
    let others: HashSet<_> = members.iter().copied().filter(|m| *m != me).collect();
    assert_eq!(f.relay(&members), others);
    assert!(f.qsendrecsigs(&relay_peer));

    let f = fixture(Some(0), base_scan(&members));
    initialized(&f, pro(77), false, false);
    assert!(f.registry.quorums(dip24()).is_empty());
}

/// DK1 `add_probes` probes nothing while spork 23 is off for the type, and every member but me
/// once it is on, into NetCore's pending probes.
// Core: llmq/net_dkg.cpp:807-809, :815-818, :842 at v24.0.0; llmq/options.cpp:44-47
#[test]
fn dk1_add_probes_follows_spork_23() {
    let members = members(4);
    let f = fixture(None, base_scan(&members));
    add_probes(&f, pro(1));
    assert!(pending_probes(&f).is_empty());
    f.sporks.update_spork(SporkId::SPORK_23_QUORUM_POSE, 0).unwrap();
    add_probes(&f, pro(1));
    assert_eq!(pending_probes(&f), HashSet::from([pro(2), pro(3), pro(4)]));
}

/// DK1 `add_probes` skips me and every member whose last outbound success is under 600 s before
/// the node clock's now: 599 s skips, 600 s and 601 s probe.
// Core: llmq/net_dkg.cpp:812, :816-823 at v24.0.0
#[test]
fn dk1_add_probes_skips_me_and_recent_outbound() {
    let members = members(4);
    let f = fixture(None, base_scan(&members));
    f.sporks.update_spork(SporkId::SPORK_23_QUORUM_POSE, 0).unwrap();
    let now = f.sim.clock.node_now().0;
    for (n, ago) in [(2, 599), (3, 600), (4, 601)] {
        f.metaman.set_last_outbound_success(&ProTxHash::from(pro(n)), now - ago);
    }
    add_probes(&f, pro(1));
    assert_eq!(pending_probes(&f), HashSet::from([pro(3), pro(4)]));
}

/// DK1 `add_probes` with a metaman whose cache did not load probes nothing and says so, where a
/// valid one probes. Core asserts `mn_metaman.IsValid()`.
// Core: llmq/net_dkg.cpp:805 at v24.0.0
#[test]
fn dk1_add_probes_needs_a_valid_metaman() {
    let members = members(3);
    for valid in [false, true] {
        let f = fixture_meta(None, base_scan(&members), valid);
        f.sporks.update_spork(SporkId::SPORK_23_QUORUM_POSE, 0).unwrap();
        let ((), lines) = logged(|| add_probes(&f, pro(1)));
        let said = lines.iter().any(|(_, l)| l.contains("mn_metaman.IsValid() is false"));
        assert_eq!(said, !valid);
        let want = if valid {
            HashSet::from([pro(2), pro(3)])
        } else {
            HashSet::new()
        };
        assert_eq!(pending_probes(&f), want, "valid {valid}");
    }
}

/// DK1 with `we_are_member` false: ensure still applies (here a watching outsider's watch
/// connection) and no probe is queued, where a member's call probes.
// Core: llmq/net_dkg.cpp:880-888 at v24.0.0
#[test]
fn dk1_not_a_member_ensures_without_probes() {
    let members = members(4);
    for we_are_member in [false, true] {
        let f = fixture(None, base_scan(&members));
        f.sporks.update_spork(SporkId::SPORK_23_QUORUM_POSE, 0).unwrap();
        initialized(&f, pro(77), true, we_are_member);
        assert!(f.registry.has(dip24(), &at(24)), "{we_are_member}");
        assert_eq!(pending_probes(&f).is_empty(), !we_are_member);
    }
}

/// A tip source that publishes `first` at its first load and an empty list after, counting loads.
struct Moving {
    first: TipListRef,
    loads: Mutex<usize>,
}

impl TipSource for Moving {
    fn load(&self) -> TipListRef {
        let mut loads = self.loads.lock().unwrap();
        *loads += 1;
        if *loads == 1 {
            self.first.clone()
        } else {
            tip_list_ref()
        }
    }
}

/// A valid masternode `pro` at 10.0.0.`pro[0]`:19999.
fn listed(pro: [u8; 32]) -> Arc<dash_dml::Dmn> {
    dash_dml::testkit::dmn_with(pro[0], dash_dml::MnType::Regular, |dmn, cold| {
        dmn.pro_tx_hash = pro.into();
        let service = Service::new(NetAddr::ipv4([10, 0, 0, pro[0]]), 19999);
        cold.net_info = Arc::new(NetInfo::Legacy(NetInfoV1::new(ServiceV1::new(
            AddrV1::from_bytes(service.addr.v1_bytes()),
            service.port,
        ))));
    })
}

/// DK1 loads the tip list once: ensure's and the probes' member blocks both name the addresses
/// of the list the first load published, though the source publishes an empty list after it.
// Core: llmq/net_dkg.cpp:880-887 at v24.0.0; llmq/net_quorum.cpp:813-827; llmq/net_dkg.cpp:827-840
#[test]
fn dk1_both_steps_see_one_tip_list() {
    let members = members(4);
    let mut list = MnList::default_pre_dip3();
    for m in &members {
        list.insert_for_tests(listed(*m));
    }
    let moving = Arc::new(Moving {
        first: TipListRef {
            list: Arc::new(list),
            bls: BlsCtx::BASIC,
        },
        loads: Mutex::new(0),
    });
    let tip = TipMnList::new(Arc::clone(&moving) as _);
    let f = fixture_with(Some(0), base_scan(&members), true, tip);
    f.sporks.update_spork(SporkId::SPORK_23_QUORUM_POSE, 0).unwrap();
    let ((), lines) = logged(|| initialized(&f, pro(1), false, true));
    assert_eq!(*moving.loads.lock().unwrap(), 1);
    let block = |prefix: &str| -> String {
        lines.iter().map(|(_, l)| l).find(|l| l.starts_with(prefix)).cloned().unwrap_or_default()
    };
    let ensured = block("EnsureQuorumConnections -- adding masternodes");
    let probed = block("AddQuorumProbeConnections -- adding masternodes");
    for message in [&ensured, &probed] {
        assert!(message.contains(":19999)"), "{lines:?}");
        assert!(!message.contains("not in valid MN set anymore"), "{lines:?}");
    }
}
