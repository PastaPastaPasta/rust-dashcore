use std::collections::HashSet;

use dash_dml::{MnList, ProTxHash};
use dash_netaddr::Service;
use dash_params::{LlmqParams, LlmqType};
use dash_primitives::NITrait;
use dash_types::Numeric;

use super::deterministic_outbound;
use crate::tip::TipListRef;

pub type ConnectionSet = HashSet<[u8; 32]>;

// Core: llmq/net_quorum.cpp:444-456, :774-780
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuorumInfo {
    pub llmq_type: LlmqType,
    pub quorum_hash: [u8; 32],
    pub base_hash: [u8; 32],
    pub base_height: i32,
    pub members: Vec<[u8; 32]>,
}

// Core: llmq/net_quorum.cpp:379, :392, :441-456
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    None,
    Masternode {
        pro_tx_hash: Option<[u8; 32]>,
        watching: bool,
    },
    Observer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuorumTip {
    pub height: i32,
    pub hash: [u8; 32],
}

// Core: net.cpp:4413-4425; llmq/net_quorum.cpp:411, :814
pub struct RegistryView<'a> {
    pub quorums: &'a dyn Fn(LlmqType) -> ConnectionSet,
    pub has: &'a dyn Fn(LlmqType, &[u8; 32]) -> bool,
}

// Core: llmq/net_quorum.cpp:819-824
pub trait ValidAddr {
    fn primary_service(&self, pro_tx_hash: &[u8; 32]) -> Option<String>;
}

impl ValidAddr for MnList {
    fn primary_service(&self, pro_tx_hash: &[u8; 32]) -> Option<String> {
        self.get_valid(&ProTxHash::from(*pro_tx_hash)).map(|dmn| {
            dmn.cold
                .net_info
                .primary()
                .map(|service| service.to_string())
                .unwrap_or_else(|| Service::default().to_string())
        })
    }
}

impl ValidAddr for TipListRef {
    fn primary_service(&self, pro_tx_hash: &[u8; 32]) -> Option<String> {
        self.list.primary_service(pro_tx_hash)
    }
}

// Core: llmq/utils.cpp:754-853
pub trait QuorumTopology {
    fn quorum_connections(
        &self,
        params: &LlmqParams,
        members: &[[u8; 32]],
        me: [u8; 32],
        only_outbound: bool,
        all_members_connected: bool,
    ) -> ConnectionSet;

    fn quorum_relay_members(
        &self,
        members: &[[u8; 32]],
        me: [u8; 32],
        only_outbound: bool,
    ) -> ConnectionSet;

    fn watch_connections(
        &self,
        seed: &[u8; 32],
        llmq_type: LlmqType,
        base: [u8; 32],
        member_count: usize,
        count: usize,
    ) -> HashSet<usize>;
}

/// Implements Core's mesh predicate (llmq/utils.cpp:757-773).
/// Ring and watch topology (llmq/utils.cpp:778-853) are not implemented.
#[derive(Default)]
pub struct InterimQuorumTopology;

impl QuorumTopology for InterimQuorumTopology {
    // Core: llmq/utils.cpp:757-775
    fn quorum_connections(
        &self,
        _params: &LlmqParams,
        members: &[[u8; 32]],
        me: [u8; 32],
        only_outbound: bool,
        all_members_connected: bool,
    ) -> ConnectionSet {
        if !all_members_connected {
            return self.quorum_relay_members(members, me, only_outbound);
        }
        members
            .iter()
            .copied()
            .filter(|remote| {
                *remote != me && (!only_outbound || deterministic_outbound(me, *remote) == *remote)
            })
            .collect()
    }

    fn quorum_relay_members(&self, _: &[[u8; 32]], _: [u8; 32], _: bool) -> ConnectionSet {
        ConnectionSet::new()
    }

    fn watch_connections(
        &self,
        _: &[u8; 32],
        _: LlmqType,
        _: [u8; 32],
        _: usize,
        _: usize,
    ) -> HashSet<usize> {
        HashSet::new()
    }
}

// Core: llmq/net_quorum.cpp:408-456; llmq/options.cpp:50-62
pub struct CheckCtx<'a> {
    pub llmq_type: LlmqType,
    pub params: &'a LlmqParams,
    pub tip: QuorumTip,
    pub ancestor: &'a dyn Fn(i32) -> [u8; 32],
    pub rotation_enabled: bool,
    pub dip24_is_type: LlmqType,
    pub all_members_connected: bool,
    pub topology: &'a dyn QuorumTopology,
    pub watch_seed: &'a [u8; 32],
    pub registry: RegistryView<'a>,
    pub tip_list: &'a dyn ValidAddr,
}

#[derive(Default, Debug, PartialEq, Eq)]
pub struct Ensure {
    pub ensured: bool,
    pub nodes: Option<ConnectionSet>,
    pub relay: Option<ConnectionSet>,
    pub log: Vec<String>,
}

// Core: llmq/net_quorum.cpp:766-834
pub fn ensure(
    ctx: &CheckCtx<'_>,
    q: &QuorumInfo,
    me: [u8; 32],
    is_mn: bool,
    watch: bool,
) -> Ensure {
    if (!is_mn && !watch) || q.members.is_empty() {
        return Ensure::default();
    }
    let member = q.members.contains(&me);
    if !member && !watch {
        return Ensure::default();
    }
    let mut log = vec![format!(
        "EnsureQuorumConnections -- isMember={} for quorum {}:",
        u8::from(member),
        hex(&q.base_hash)
    )];
    let (nodes, relay) = if member {
        let nodes = ctx.topology.quorum_connections(
            ctx.params,
            &q.members,
            me,
            true,
            ctx.all_members_connected,
        );
        let relay = if ctx.all_members_connected {
            q.members.iter().copied().filter(|remote| *remote != me).collect()
        } else {
            ctx.topology.quorum_relay_members(&q.members, me, true)
        };
        (nodes, relay)
    } else {
        let nodes = watch_nodes(ctx, q);
        (nodes.clone(), nodes)
    };
    if !nodes.is_empty() && !(ctx.registry.has)(ctx.llmq_type, &q.base_hash) {
        log.push(format!(
            "EnsureQuorumConnections -- adding masternodes quorum connections for quorum {}:",
            hex(&q.base_hash)
        ));
        for node in &nodes {
            let addr = ctx
                .tip_list
                .primary_service(node)
                .unwrap_or_else(|| "not in valid MN set anymore".into());
            log.push(format!("  {} ({addr})", hex(node)));
        }
    }
    Ensure {
        ensured: true,
        nodes: (!nodes.is_empty()).then_some(nodes),
        relay: (!relay.is_empty()).then_some(relay),
        log,
    }
}

fn watch_nodes(ctx: &CheckCtx<'_>, q: &QuorumInfo) -> ConnectionSet {
    if q.members.is_empty() {
        return ConnectionSet::new();
    }
    ctx.topology
        .watch_connections(ctx.watch_seed, ctx.llmq_type, q.base_hash, q.members.len(), 1)
        .into_iter()
        .filter_map(|index| q.members.get(index).copied())
        .collect()
}

// Core: llmq/net_quorum.cpp:408-436
pub fn quorums_to_delete(ctx: &CheckCtx<'_>) -> ConnectionSet {
    deletion_plan(ctx, &mut Vec::new())
}

fn deletion_plan(ctx: &CheckCtx<'_>, log: &mut Vec<String>) -> ConnectionSet {
    let mut deletable = (ctx.registry.quorums)(ctx.llmq_type);
    let cycle_index = ctx.tip.height % i32::from(ctx.params.dkg_interval);
    let cycle_base = ctx.tip.height - cycle_index;
    if ctx.rotation_enabled {
        let mut kept = String::new();
        for i in 0..i32::from(ctx.params.signing_active_quorum_count) {
            if i <= cycle_index {
                let height = cycle_base + i;
                let hash = (ctx.ancestor)(height);
                kept.push_str(&format!("{height}:{} | ", hex(&hash)));
                deletable.remove(&hash);
            }
        }
        log.push(format!(
            "{}keeping mn quorum connections for rotated quorums: [{kept}]",
            prefix(ctx, "GetQuorumsToDelete")
        ));
    } else {
        let hash = (ctx.ancestor)(cycle_base);
        deletable.remove(&hash);
        log.push(format!(
            "{}keeping mn quorum connections for quorum: [{cycle_base}:{}]",
            prefix(ctx, "GetQuorumsToDelete"),
            hex(&hash)
        ));
    }
    deletable
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegistryOp {
    SetNodes {
        llmq_type: LlmqType,
        hash: [u8; 32],
        nodes: ConnectionSet,
    },
    SetRelay {
        llmq_type: LlmqType,
        hash: [u8; 32],
        members: ConnectionSet,
    },
    Remove {
        llmq_type: LlmqType,
        hash: [u8; 32],
    },
}

#[derive(Default, Debug, PartialEq, Eq)]
pub struct Check {
    pub ops: Vec<RegistryOp>,
    pub log: Vec<String>,
}

// Core: llmq/net_quorum.cpp:438-497
/// `scan` is `ScanQuorums(type, tip, keepOldConnections)` (llmq/net_quorum.cpp:444); ensured
/// quorums in it are kept off the removal list (:457).
pub fn check(ctx: &CheckCtx<'_>, role: Role, scan: Vec<QuorumInfo>) -> Check {
    let (me, is_mn, watching) = match role {
        Role::None => return Check::default(),
        Role::Observer => ([0; 32], false, true),
        Role::Masternode {
            pro_tx_hash,
            watching,
        } => (pro_tx_hash.unwrap_or([0; 32]), true, watching),
    };
    let mut result = Check::default();
    let mut deletable = deletion_plan(ctx, &mut result.log);
    let watch_other_is =
        is_mn && ctx.llmq_type == ctx.dip24_is_type && scan.iter().any(|q| q.members.contains(&me));
    for q in scan {
        let ensured = ensure(ctx, &q, me, is_mn, watching);
        result.log.extend(ensured.log);
        if ensured.ensured {
            if let Some(nodes) = ensured.nodes {
                result.ops.push(RegistryOp::SetNodes {
                    llmq_type: ctx.llmq_type,
                    hash: q.base_hash,
                    nodes,
                });
            }
            if let Some(members) = ensured.relay {
                result.ops.push(RegistryOp::SetRelay {
                    llmq_type: ctx.llmq_type,
                    hash: q.base_hash,
                    members,
                });
            }
            if deletable.remove(&q.quorum_hash) {
                result.log.push(keeping(ctx, &q, false));
            }
        } else if watch_other_is && !q.members.contains(&me) {
            let nodes = watch_nodes(ctx, &q);
            if !nodes.is_empty() {
                if !(ctx.registry.has)(ctx.llmq_type, &q.base_hash) {
                    result.log.push(format!(
                        "{}adding mn inter-quorum connections for quorum: [{}:{}]",
                        prefix(ctx, "CheckQuorumConnections"),
                        q.base_height,
                        hex(&q.base_hash)
                    ));
                    result.ops.push(RegistryOp::SetNodes {
                        llmq_type: ctx.llmq_type,
                        hash: q.base_hash,
                        nodes: nodes.clone(),
                    });
                    result.ops.push(RegistryOp::SetRelay {
                        llmq_type: ctx.llmq_type,
                        hash: q.base_hash,
                        members: nodes,
                    });
                }
                if deletable.remove(&q.quorum_hash) {
                    result.log.push(keeping(ctx, &q, true));
                }
            }
        }
    }
    for hash in deletable {
        result.log.push(format!(
            "NetQuorum::CheckQuorumConnections -- removing masternodes quorum connections for quorum {}:", hex(&hash)
        ));
        result.ops.push(RegistryOp::Remove {
            llmq_type: ctx.llmq_type,
            hash,
        });
    }
    result
}

fn prefix(ctx: &CheckCtx<'_>, function: &str) -> String {
    format!("NetQuorum::{function} -- llmqType[{}] h[{}] ", ctx.llmq_type.to_base(), ctx.tip.height)
}

fn keeping(ctx: &CheckCtx<'_>, q: &QuorumInfo, inter: bool) -> String {
    let kind = if inter {
        "inter-quorum"
    } else {
        "quorum"
    };
    format!(
        "{}keeping mn {kind} connections for quorum: [{}:{}]",
        prefix(ctx, "CheckQuorumConnections"),
        q.base_height,
        hex(&q.base_hash)
    )
}

fn hex(hash: &[u8; 32]) -> String {
    hash.iter().rev().map(|b| format!("{b:02x}")).collect()
}
