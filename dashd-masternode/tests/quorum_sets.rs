use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::Arc;

use dash_consensus::BlsCtx;
use dash_dml::testkit::{dmn_with, pro_tx};
use dash_dml::{MnList, MnType};
use dash_primitives::{AddrV1, NetInfo, NetInfoV1, NetInfoV2, ServiceV1};
use dashd_masternode::tip::TipListRef;

use dash_params::{ChainParamsBuilder, LlmqParams, LlmqType, Network, Release};
use dashd_masternode::mnconn::{
    Check, CheckCtx, ConnectionSet, InterimQuorumTopology, QuorumInfo, QuorumTip, QuorumTopology,
    RegistryOp, RegistryView, Role, ValidAddr, check, ensure, quorums_to_delete,
};

const TYPE: LlmqType = LlmqType::LlmqTest;
const SEED: [u8; 32] = [99; 32];

fn hash(n: u8) -> [u8; 32] {
    let mut hash = [0; 32];
    hash[0] = n;
    hash
}

fn set(ns: &[u8]) -> ConnectionSet {
    ns.iter().map(|n| hash(*n)).collect()
}

fn quorum(n: u8, members: &[u8]) -> QuorumInfo {
    QuorumInfo {
        llmq_type: TYPE,
        quorum_hash: hash(n),
        base_hash: hash(n),
        base_height: i32::from(n),
        members: members.iter().map(|n| hash(*n)).collect(),
    }
}

fn mn(n: Option<u8>, watching: bool) -> Role {
    Role::Masternode {
        pro_tx_hash: n.map(hash),
        watching,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Call {
    Connections(u16, Vec<[u8; 32]>, [u8; 32], bool, bool),
    Relay(Vec<[u8; 32]>, [u8; 32], bool),
    Watch([u8; 32], LlmqType, [u8; 32], usize, usize),
}

#[derive(Default)]
struct Topology {
    nodes: ConnectionSet,
    relay: ConnectionSet,
    watch: HashSet<usize>,
    calls: RefCell<Vec<Call>>,
}

impl QuorumTopology for Topology {
    fn quorum_connections(
        &self,
        params: &LlmqParams,
        members: &[[u8; 32]],
        me: [u8; 32],
        outbound: bool,
        all: bool,
    ) -> ConnectionSet {
        self.calls.borrow_mut().push(Call::Connections(
            params.size,
            members.to_vec(),
            me,
            outbound,
            all,
        ));
        self.nodes.clone()
    }

    fn quorum_relay_members(
        &self,
        members: &[[u8; 32]],
        me: [u8; 32],
        outbound: bool,
    ) -> ConnectionSet {
        self.calls.borrow_mut().push(Call::Relay(members.to_vec(), me, outbound));
        self.relay.clone()
    }

    fn watch_connections(
        &self,
        seed: &[u8; 32],
        llmq_type: LlmqType,
        base: [u8; 32],
        members: usize,
        count: usize,
    ) -> HashSet<usize> {
        assert_ne!(members, 0);
        self.calls.borrow_mut().push(Call::Watch(*seed, llmq_type, base, members, count));
        self.watch.clone()
    }
}

struct Addresses;

impl ValidAddr for Addresses {
    fn primary_service(&self, pro: &[u8; 32]) -> Option<String> {
        if *pro == hash(2) {
            Some("127.0.0.1:1234".into())
        } else {
            None
        }
    }
}

fn regtest_params() -> LlmqParams {
    let setup = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap();
    *setup.consensus.llmq(TYPE).unwrap()
}

fn with_ctx(topology: &dyn QuorumTopology, registered: &[u8], run: impl FnOnce(CheckCtx<'_>)) {
    let params = regtest_params();
    let quorums = |t| {
        assert_eq!(t, TYPE);
        set(registered)
    };
    let has = |t, h: &[u8; 32]| {
        assert_eq!(t, TYPE);
        registered.iter().any(|n| hash(*n) == *h)
    };
    run(CheckCtx {
        llmq_type: TYPE,
        params: &params,
        tip: QuorumTip {
            height: 24,
            hash: hash(24),
        },
        ancestor: &|height| hash(u8::try_from(height).unwrap()),
        rotation_enabled: false,
        dip24_is_type: LlmqType::LlmqTestDip0024,
        all_members_connected: false,
        topology,
        watch_seed: &SEED,
        registry: RegistryView {
            quorums: &quorums,
            has: &has,
        },
        tip_list: &Addresses,
    });
}

fn nodes(n: u8, values: &[u8]) -> RegistryOp {
    RegistryOp::SetNodes {
        llmq_type: TYPE,
        hash: hash(n),
        nodes: set(values),
    }
}

fn relay(n: u8, values: &[u8]) -> RegistryOp {
    RegistryOp::SetRelay {
        llmq_type: TYPE,
        hash: hash(n),
        members: set(values),
    }
}

fn remove(n: u8) -> RegistryOp {
    RegistryOp::Remove {
        llmq_type: TYPE,
        hash: hash(n),
    }
}

// Core: llmq/net_quorum.cpp:770-787
#[test]
fn core_ensure_eligibility_table() {
    for (members, me, is_mn, watch, expected) in [
        (vec![1, 2], 1, false, false, false),
        (vec![], 1, true, true, false),
        (vec![2, 3], 1, true, false, false),
        (vec![1, 2], 1, true, false, true),
        (vec![1, 2], 1, false, true, true),
        (vec![2, 3], 1, true, true, true),
        (vec![2, 3], 0, false, true, true),
    ] {
        let topology = Topology::default();
        with_ctx(&topology, &[], |ctx| {
            let result = ensure(&ctx, &quorum(8, &members), hash(me), is_mn, watch);
            assert_eq!(result.ensured, expected, "{members:?} {me} {is_mn} {watch}");
            assert_eq!(!result.log.is_empty(), expected);
            assert_eq!(!topology.calls.borrow().is_empty(), expected);
            assert_eq!(result.nodes, None);
            assert_eq!(result.relay, None);
        });
    }
}

// Core: llmq/net_quorum.cpp:791-811; llmq/utils.cpp:789-793
#[test]
fn core_ensure_topology_arguments_and_relay_table() {
    for all in [false, true] {
        let topology = Topology {
            nodes: set(&[2]),
            relay: set(&[3]),
            ..Topology::default()
        };
        with_ctx(&topology, &[], |mut ctx| {
            ctx.all_members_connected = all;
            let q = quorum(8, &[3, 1, 2]);
            let result = ensure(&ctx, &q, hash(1), true, false);
            assert_eq!(result.nodes, Some(set(&[2])));
            assert_eq!(
                result.relay,
                Some(if all {
                    set(&[2, 3])
                } else {
                    set(&[3])
                })
            );
            let mut expected =
                vec![Call::Connections(ctx.params.size, q.members.clone(), hash(1), true, all)];
            if !all {
                expected.push(Call::Relay(q.members, hash(1), true));
            }
            assert_eq!(*topology.calls.borrow(), expected);
        });
    }
    for all in [false, true] {
        with_ctx(&InterimQuorumTopology, &[], |mut ctx| {
            ctx.all_members_connected = all;
            let result = ensure(&ctx, &quorum(8, &[1]), hash(1), true, false);
            assert!(result.ensured);
            assert_eq!(result.nodes, None);
            assert_eq!(result.relay, None);
        });
    }
    let topology = Topology {
        watch: HashSet::from([1]),
        ..Topology::default()
    };
    with_ctx(&topology, &[], |ctx| {
        let result = ensure(&ctx, &quorum(8, &[3, 2, 1]), hash(9), true, true);
        assert_eq!(result.nodes, Some(set(&[2])));
        assert_eq!(result.relay, result.nodes);
        assert_eq!(*topology.calls.borrow(), vec![Call::Watch(SEED, TYPE, hash(8), 3, 1)]);
    });
}

// Core: llmq/net_quorum.cpp:813-833
#[test]
fn core_ensure_logs_and_existing_registry_table() {
    for existing in [false, true] {
        let topology = Topology {
            nodes: set(&[2, 3]),
            ..Topology::default()
        };
        with_ctx(
            &topology,
            if existing {
                &[8]
            } else {
                &[]
            },
            |ctx| {
                let result = ensure(&ctx, &quorum(8, &[1, 2, 3]), hash(1), true, false);
                assert_eq!(result.nodes, Some(set(&[2, 3])));
                let mut expected =
                    vec![format!("EnsureQuorumConnections -- isMember=1 for quorum {:064x}:", 8)];
                if !existing {
                    expected.extend([
                    format!("EnsureQuorumConnections -- adding masternodes quorum connections for quorum {:064x}:", 8),
                    format!("  {:064x} (127.0.0.1:1234)", 2),
                    format!("  {:064x} (not in valid MN set anymore)", 3),
                ]);
                }
                let mut actual = result.log;
                actual.sort();
                expected.sort();
                assert_eq!(actual, expected);
            },
        );
    }
}

// Core: llmq/utils.cpp:757-775
#[test]
fn core_interim_mesh_predicate_table() {
    let topology = InterimQuorumTopology;
    let params = regtest_params();
    let members = [hash(0), hash(1), hash(2), hash(3)];
    // Core: llmq/utils.cpp:732-752; contrib/node/p7-captures/outbound.py
    for (me, expected) in [(0, vec![3]), (1, vec![0, 3]), (2, vec![0, 1, 3]), (3, vec![])] {
        assert_eq!(
            topology.quorum_connections(&params, &members, hash(me), true, true),
            set(&expected)
        );
        assert_eq!(
            topology.quorum_connections(&params, &members, hash(me), false, true),
            members.into_iter().filter(|m| *m != hash(me)).collect()
        );
        for outbound in [false, true] {
            assert!(
                topology
                    .quorum_connections(&params, &members, hash(me), outbound, false)
                    .is_empty()
            );
            assert!(topology.quorum_relay_members(&members, hash(me), outbound).is_empty());
        }
    }
    assert!(topology.watch_connections(&SEED, TYPE, hash(8), members.len(), 1).is_empty());
}

// Core: llmq/net_quorum.cpp:408-436
#[test]
fn core_quorums_to_delete_rotation_table() {
    for rotation in [false, true] {
        for height in [0, 1, 2, 3, 7, 8, 9] {
            with_ctx(&Topology::default(), &(0..=16).collect::<Vec<_>>(), |ctx| {
                let params = LlmqParams {
                    dkg_interval: 8,
                    signing_active_quorum_count: 3,
                    ..*ctx.params
                };
                let calls = RefCell::new(Vec::new());
                let ancestor = |height: i32| {
                    calls.borrow_mut().push(height);
                    hash(u8::try_from(height).unwrap())
                };
                let ctx = CheckCtx {
                    params: &params,
                    ancestor: &ancestor,
                    tip: QuorumTip {
                        height,
                        ..ctx.tip
                    },
                    rotation_enabled: rotation,
                    ..ctx
                };
                let base = height - height % 8;
                let kept: Vec<_> = if rotation {
                    (0..3).filter(|i| *i <= height % 8).map(|i| base + i).collect()
                } else {
                    vec![base]
                };
                let expected =
                    (0..=16).filter(|n| !kept.contains(&i32::from(*n))).map(hash).collect();
                assert_eq!(quorums_to_delete(&ctx), expected, "{height} {rotation}");
                assert_eq!(*calls.borrow(), kept);
                let result = check(&ctx, Role::Observer, vec![]);
                let expected_line = if rotation {
                    let kept: String = kept.iter().map(|h| format!("{h}:{h:064x} | ")).collect();
                    format!(
                        "NetQuorum::GetQuorumsToDelete -- llmqType[100] h[{height}] keeping mn quorum connections for rotated quorums: [{kept}]"
                    )
                } else {
                    format!(
                        "NetQuorum::GetQuorumsToDelete -- llmqType[100] h[{height}] keeping mn quorum connections for quorum: [{base}:{base:064x}]"
                    )
                };
                assert_eq!(result.log[0], expected_line);
            });
        }
    }
}

// Core: llmq/net_quorum.cpp:379, :392, :441-462, :492-496
#[test]
fn core_check_roles_and_operation_order_table() {
    for (role, expected) in [
        (Role::None, vec![]),
        (mn(Some(1), false), vec![nodes(8, &[2]), relay(8, &[3]), remove(9)]),
        (mn(None, false), vec![remove(8), remove(9)]),
        (mn(Some(7), true), vec![nodes(8, &[2]), relay(8, &[2]), remove(9)]),
        (Role::Observer, vec![nodes(8, &[2]), relay(8, &[2]), remove(9)]),
    ] {
        let topology = Topology {
            nodes: set(&[2]),
            relay: set(&[3]),
            watch: HashSet::from([1]),
            ..Topology::default()
        };
        with_ctx(&topology, &[8, 9], |ctx| {
            let result = check(&ctx, role, vec![quorum(8, &[1, 2, 3])]);
            let split = result
                .ops
                .iter()
                .position(|op| matches!(op, RegistryOp::Remove { .. }))
                .unwrap_or(result.ops.len());
            assert_eq!(&result.ops[..split], &expected[..split]);
            let remaining: HashSet<_> = result.ops[split..]
                .iter()
                .map(|op| match op {
                    RegistryOp::Remove {
                        hash,
                        ..
                    } => *hash,
                    _ => panic!(),
                })
                .collect();
            let expected_remaining: HashSet<_> = expected[split..]
                .iter()
                .map(|op| match op {
                    RegistryOp::Remove {
                        hash,
                        ..
                    } => *hash,
                    _ => panic!(),
                })
                .collect();
            assert_eq!(remaining, expected_remaining);
            if role == Role::None {
                assert_eq!(result, Check::default());
                assert!(topology.calls.borrow().is_empty());
            } else if !matches!(
                role,
                Role::Masternode {
                    pro_tx_hash: None,
                    ..
                }
            ) {
                assert!(result.log.contains(&format!("NetQuorum::CheckQuorumConnections -- llmqType[100] h[24] keeping mn quorum connections for quorum: [8:{:064x}]", 8)));
            }
        });
    }
}

// Core: llmq/net_quorum.cpp:377, :390, :441-456
#[test]
fn core_check_none_role_and_observer_watch_table() {
    let q = quorum(8, &[1, 2, 3]);
    for (role, expected_ops, expected_watch_calls) in
        [(Role::None, vec![], 0), (Role::Observer, vec![nodes(8, &[2]), relay(8, &[2])], 1)]
    {
        let topology = Topology {
            watch: HashSet::from([1]),
            ..Topology::default()
        };
        with_ctx(&topology, &[], |ctx| {
            let result = check(&ctx, role, vec![q.clone()]);
            assert_eq!(result.ops, expected_ops);
            assert_eq!(
                topology
                    .calls
                    .borrow()
                    .iter()
                    .filter(|call| matches!(call, Call::Watch(..)))
                    .count(),
                expected_watch_calls
            );
            if matches!(role, Role::None) {
                assert!(result.log.is_empty());
                assert!(topology.calls.borrow().is_empty());
            }
        });
    }
}

// Core: llmq/net_quorum.cpp:447-488
#[test]
fn core_check_inter_quorum_is_table() {
    for is_type in [false, true] {
        for present in [false, true] {
            for watched in [false, true] {
                let topology = Topology {
                    watch: if watched {
                        HashSet::from([1])
                    } else {
                        HashSet::new()
                    },
                    ..Topology::default()
                };
                with_ctx(
                    &topology,
                    if present {
                        &[8, 9]
                    } else {
                        &[8]
                    },
                    |mut ctx| {
                        if is_type {
                            ctx.dip24_is_type = TYPE;
                        }
                        let result = check(
                            &ctx,
                            mn(Some(1), false),
                            vec![quorum(8, &[1]), quorum(9, &[3, 2])],
                        );
                        let expected = if is_type && watched && !present {
                            vec![nodes(9, &[2]), relay(9, &[2])]
                        } else if present && !(is_type && watched) {
                            vec![remove(9)]
                        } else {
                            vec![]
                        };
                        assert_eq!(result.ops, expected, "{is_type} {present} {watched}");
                        assert_eq!(
                            topology
                                .calls
                                .borrow()
                                .iter()
                                .filter(|c| matches!(c, Call::Watch(..)))
                                .count(),
                            usize::from(is_type)
                        );
                        if is_type && watched && present {
                            assert!(result.log.contains(&format!("NetQuorum::CheckQuorumConnections -- llmqType[100] h[24] keeping mn inter-quorum connections for quorum: [9:{:064x}]", 9)));
                        }
                    },
                );
            }
        }
    }
    let topology = Topology {
        watch: HashSet::from([0]),
        ..Topology::default()
    };
    with_ctx(&topology, &[9], |mut ctx| {
        ctx.dip24_is_type = TYPE;
        assert_eq!(check(&ctx, mn(Some(1), false), vec![quorum(9, &[2])]).ops, vec![remove(9)]);
        assert!(topology.calls.borrow().is_empty());
    });
}

// Core: llmq/net_quorum.cpp:457, :472, :482, :813-834
#[test]
fn core_check_empty_sets_preserve_registry_and_base_hash_keys() {
    for (node_set, relay_set, expected) in [
        (vec![], vec![], vec![]),
        (vec![2], vec![], vec![nodes(8, &[2])]),
        (vec![], vec![3], vec![relay(8, &[3])]),
    ] {
        let topology = Topology {
            nodes: set(&node_set),
            relay: set(&relay_set),
            ..Topology::default()
        };
        with_ctx(&topology, &[8], |ctx| {
            assert_eq!(check(&ctx, mn(Some(1), false), vec![quorum(8, &[1, 2, 3])]).ops, expected);
        });
    }
    for all in [false, true] {
        for role in [mn(Some(1), false), Role::Observer] {
            with_ctx(&InterimQuorumTopology, &[8, 9], |mut ctx| {
                ctx.all_members_connected = all;
                let mut q = quorum(8, &[1]);
                q.base_hash = hash(10);
                assert_eq!(check(&ctx, role, vec![q]).ops, vec![remove(9)]);
            });
        }
    }
    let topology = Topology {
        nodes: set(&[2]),
        relay: set(&[3]),
        ..Topology::default()
    };
    with_ctx(&topology, &[8], |ctx| {
        let mut q = quorum(8, &[1, 2, 3]);
        q.base_hash = hash(10);
        let result = check(&ctx, mn(Some(1), false), vec![q]);
        assert_eq!(result.ops, vec![nodes(10, &[2]), relay(10, &[3])]);
        assert!(result.log.iter().any(|line| line == &format!("EnsureQuorumConnections -- adding masternodes quorum connections for quorum {:064x}:", 10)));
    });
}

// Core: llmq/net_quorum.cpp:774-777; llmq/utils.cpp:849-850
#[test]
fn core_check_empty_members_do_not_call_watch() {
    let topology = Topology {
        watch: HashSet::from([0]),
        ..Topology::default()
    };
    with_ctx(&topology, &[8, 9], |mut ctx| {
        ctx.dip24_is_type = TYPE;
        assert_eq!(
            check(&ctx, mn(Some(1), false), vec![quorum(8, &[1]), quorum(9, &[])]).ops,
            vec![remove(9)]
        );
        assert!(!topology.calls.borrow().iter().any(|call| matches!(call, Call::Watch(..))));
    });
}

// Core: llmq/net_quorum.cpp:819-824; evo/netinfo.cpp:364-370, :589-603
#[test]
fn core_valid_address_uses_tip_primary_service() {
    let mut list = MnList::default_pre_dip3();
    for n in [1, 2, 3] {
        list.insert_for_tests(dmn_with(n, MnType::Regular, |dmn, cold| {
            cold.net_info = Arc::new(if n == 3 {
                cold.version = 3;
                NetInfo::Extended(NetInfoV2::new(1, Vec::new()))
            } else {
                let mut ip = [0; 16];
                ip[10..12].copy_from_slice(&[255, 255]);
                ip[12..].copy_from_slice(&[127, 0, 0, n]);
                NetInfo::Legacy(NetInfoV1::new(ServiceV1::new(AddrV1::from_bytes(ip), 1234)))
            });
            if n == 2 {
                dmn.hot.ban_if_not_banned(100);
            }
        }));
    }
    let tip = TipListRef {
        list: Arc::new(list),
        bls: BlsCtx::BASIC,
    };
    for (n, expected) in [(1, Some("127.0.0.1:1234")), (2, None), (3, Some("[::]:0")), (4, None)] {
        assert_eq!(tip.primary_service(pro_tx(n).as_bytes()).as_deref(), expected);
    }
}
