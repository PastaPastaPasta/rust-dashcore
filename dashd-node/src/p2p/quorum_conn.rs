//! P7 W3 and the DK1 adapter: Core's `CheckQuorumConnections`, `EnsureQuorumConnections` and
//! `AddQuorumProbeConnections` over the node's one quorum registry and its network core.
//!
//! Nothing here subscribes to tip events. Step 7c installs [`NodeQuorumConnections`] as QM1b-7's
//! `NetQuorumTip`'s [`QuorumConnectionsCheck`] (`p2p::quorum_net::spawn_tip_listener`) and as
//! DK1's round driver's [`DkgConnections`], built from `Net::quorum_nodes`, `Net::core`, step 7a's
//! spork manager, step 6's metaman, the published tip list, the node clock and V's handle. Core
//! cites are v24.0.2 unless marked v24.0.0.

use std::sync::Arc;

use dash_clock::Clock;
use dash_evo::types::ProTxHash;
use dash_p2p::{CommandString, NetCore};
use dash_p2p_core::QSendRecSigs;
use dash_params::{ChainSetup, LlmqParams, LlmqType};
use dash_primitives::BlockHash;
use dashd_chainstate::chain::{ChainHandle, no_wait_marked};
use dashd_masternode::meta::MasternodeMetaMan;
use dashd_masternode::mnconn::sporks::{all_members_connected, quorum_pose_enabled};
use dashd_masternode::mnconn::{
    self, Check, CheckCtx, LlmqTopology, QuorumInfo, QuorumNodes, QuorumTip, RegistryOp,
    RegistryView, ValidAddr, check, ensure, probe_targets,
};
use dashd_masternode::sporks::SporkManager;
use dashd_masternode::tip::TipMnList;
use dashd_p2p::processing::logic::out_msg;

use super::DkgConnections;
use super::quorum_tip::{QuorumConnectionsCheck, Role, TipRef};

/// One `ScanQuorums(type, tip, keepOldConnections)` with `IsQuorumRotationEnabled(params, tip)`
/// from the same chain state.
// Core: llmq/net_quorum.cpp:413, :444; llmq/options.cpp:50-62
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QuorumScan {
    pub quorums: Vec<QuorumInfo>,
    pub rotation_enabled: bool,
}

/// The chain reads the quorum connection calls make.
pub trait QuorumScanRead: Send + Sync {
    /// The scan at `tip`; `None` when no block has the hash or the read fails.
    fn scan(&self, llmq_type: LlmqType, tip: &BlockHash, count: usize) -> Option<QuorumScan>;

    /// `tip`'s ancestor's hash at `height`; `None` when either is not in the index.
    fn ancestor(&self, tip: &BlockHash, height: i32) -> Option<[u8; 32]>;

    /// `GetAllQuorumMembers(type, base)`'s proTxHashes, in Core's order; `None` when no block
    /// has the hash or the read fails.
    // Core: llmq/net_quorum.cpp:774; llmq/net_dkg.cpp:811
    fn members(&self, llmq_type: LlmqType, base: &BlockHash) -> Option<Vec<[u8; 32]>>;
}

/// V's reads: the scan and the members through route B's `OnVBinding` in one `OnV` turn each,
/// and the ancestor on V's block-index reader. The scan and the members wait on V for that turn,
/// so the caller is never V or a no-wait thread (the signals thread); on a no-wait thread they
/// assert in debug builds and answer `None` without sending.
impl QuorumScanRead for ChainHandle {
    fn scan(&self, llmq_type: LlmqType, tip: &BlockHash, count: usize) -> Option<QuorumScan> {
        debug_assert!(!no_wait_marked(), "a quorum connection read waits on V");
        if no_wait_marked() {
            return None;
        }
        let tip = *tip;
        self.on_v(move |on_v, query| {
            let reader = query.chain.reader();
            let id = reader.lookup(&tip)?;
            let (quorums, rotation_enabled) =
                query.bind(on_v).scan_quorums_with_rotation(llmq_type, id, count).ok()?;
            let quorums = quorums
                .iter()
                .map(|q| QuorumInfo {
                    llmq_type,
                    quorum_hash: *q.commitment.quorum_hash().as_bytes(),
                    base_hash: *q.base_hash.as_bytes(),
                    base_height: reader.entry(q.base).map_or(-1, |e| e.height as i32),
                    members: q.members.iter().map(|m| *m.pro_tx_hash.as_bytes()).collect(),
                })
                .collect();
            Some(QuorumScan {
                quorums,
                rotation_enabled,
            })
        })
        .recv()
        .ok()
        .flatten()
    }

    fn ancestor(&self, tip: &BlockHash, height: i32) -> Option<[u8; 32]> {
        let reader = self.reader();
        let id = reader.ancestor(reader.lookup(tip)?, u32::try_from(height).ok()?)?;
        reader.entry(id).map(|e| *e.hash.as_bytes())
    }

    fn members(&self, llmq_type: LlmqType, base: &BlockHash) -> Option<Vec<[u8; 32]>> {
        debug_assert!(!no_wait_marked(), "a quorum connection read waits on V");
        if no_wait_marked() {
            return None;
        }
        let base = *base;
        self.on_v(move |on_v, query| {
            let id = query.chain.reader().lookup(&base)?;
            let members = query.bind(on_v).members(llmq_type, id, false).ok()?;
            Some(members.iter().map(|m| *m.pro_tx_hash.as_bytes()).collect())
        })
        .recv()
        .ok()
        .flatten()
    }
}

/// What [`NodeQuorumConnections`] reads and writes.
pub struct QuorumConnParts {
    pub params: Arc<ChainSetup>,
    pub chain: Arc<dyn QuorumScanRead>,
    pub registry: Arc<QuorumNodes>,
    pub core: Arc<NetCore>,
    pub sporks: Arc<SporkManager>,
    pub metaman: Arc<MasternodeMetaMan>,
    pub tip_list: TipMnList,
    pub clock: Arc<dyn Clock>,
    /// The process's seed, `*mnconn::watch_seed()`.
    pub watch_seed: [u8; 32],
}

/// Core's `CheckQuorumConnections`, `EnsureQuorumConnections` and `AddQuorumProbeConnections`
/// over the node's registry and network core.
pub struct NodeQuorumConnections(QuorumConnParts);

impl NodeQuorumConnections {
    pub fn new(parts: QuorumConnParts) -> NodeQuorumConnections {
        NodeQuorumConnections(parts)
    }

    /// Core's `EnsureQuorumConnections(.., is_masternode = true, quorums_watch = watch)`: the
    /// node and relay sets applied after the lines Core prints before them.
    // Core: llmq/net_quorum.cpp:766-835 at v24.0.0
    fn ensure_dkg(
        &self,
        params: &LlmqParams,
        quorum: &QuorumInfo,
        tip_list: &dyn ValidAddr,
        me: &ProTxHash,
        watch: bool,
    ) {
        let parts = &self.0;
        let llmq_type = quorum.llmq_type;
        let quorums = |llmq_type| parts.registry.quorums(llmq_type);
        let has = |llmq_type, hash: &[u8; 32]| parts.registry.has(llmq_type, hash);
        let ctx = CheckCtx {
            llmq_type,
            params,
            tip: QuorumTip {
                height: quorum.base_height,
                hash: quorum.base_hash,
            },
            ancestor: &|_| [0; 32],
            rotation_enabled: false,
            dip24_is_type: parts.params.consensus.llmq_type_dip0024_instant_send,
            all_members_connected: all_members_connected(llmq_type, &parts.sporks),
            topology: &LlmqTopology,
            watch_seed: &parts.watch_seed,
            registry: RegistryView {
                quorums: &quorums,
                has: &has,
            },
            tip_list,
        };
        let ensured = ensure(&ctx, quorum, *me.as_bytes(), true, watch);
        emit(&ensured.log);
        if let Some(nodes) = ensured.nodes {
            self.apply(RegistryOp::SetNodes {
                llmq_type,
                hash: quorum.base_hash,
                nodes,
            });
        }
        if let Some(members) = ensured.relay {
            self.apply(RegistryOp::SetRelay {
                llmq_type,
                hash: quorum.base_hash,
                members,
            });
        }
    }

    /// Core's `AddQuorumProbeConnections`: with spork 23 on for the type, every member but `me`
    /// not connected outbound in the last 600 s by the node clock, queued to probe. Core asserts
    /// the metaman is valid; here an invalid one probes nothing.
    // Core: llmq/net_dkg.cpp:800-844 at v24.0.0
    fn add_probes(&self, quorum: &QuorumInfo, tip_list: &dyn ValidAddr, me: &ProTxHash) {
        let parts = &self.0;
        // Core: llmq/net_dkg.cpp:805 at v24.0.0
        if !parts.metaman.is_valid() {
            tracing::debug!(
                target: "llmq",
                "AddQuorumProbeConnections -- mn_metaman.IsValid() is false, no probes"
            );
            return;
        }
        if !quorum_pose_enabled(quorum.llmq_type, &parts.sporks) {
            return;
        }
        let now = parts.clock.node_now().0;
        let last_outbound =
            |pro: &[u8; 32]| parts.metaman.last_outbound_success(&ProTxHash::from(*pro));
        let (targets, log) = probe_targets(
            true,
            quorum.base_hash,
            &quorum.members,
            *me.as_bytes(),
            last_outbound,
            now,
            tip_list,
        );
        emit(&log);
        if !targets.is_empty() {
            let mut targets: Vec<[u8; 32]> = targets.into_iter().collect();
            targets.sort_unstable();
            parts.core.add_pending_probes(&targets);
        }
    }

    // Core: net.cpp:4380-4411, :4459-4463
    fn apply(&self, op: RegistryOp) {
        let registry = &self.0.registry;
        match op {
            RegistryOp::SetNodes {
                llmq_type,
                hash,
                nodes,
            } => registry.set_nodes(llmq_type, hash, nodes),
            RegistryOp::SetRelay {
                llmq_type,
                hash,
                members,
            } => {
                registry.set_relay_members(llmq_type, hash, members, &[]);
                self.send_qsendrecsigs();
            }
            RegistryOp::Remove {
                llmq_type,
                hash,
            } => registry.remove(llmq_type, &hash),
        }
    }

    /// `SetMasternodeQuorumRelayMembers`' update of existing connections, after the store: each
    /// fully connected peer whose verified proTxHash is in a relay set and that has no IQR flag
    /// gets QSENDRECSIGS. The registry lock is taken per peer and released before the push.
    // Core: net.cpp:4399-4410; net.h:1353-1359, :1434; net.cpp:4875-4878
    fn send_qsendrecsigs(&self) {
        let parts = &self.0;
        for peer in parts.core.peers().iter() {
            if !peer.successfully_connected() || peer.disconnect_requested() {
                continue;
            }
            let pro_tx_hash = peer.verified_proreg_hash();
            if pro_tx_hash == [0; 32]
                || peer.masternode_iqr()
                || !parts.registry.is_relay_member(&pro_tx_hash)
            {
                continue;
            }
            parts.core.push(peer, out_msg(CommandString::QSENDRECSIGS, &QSendRecSigs::new(true)));
            peer.set_masternode_iqr(true);
        }
    }
}

/// Core's `CheckQuorumConnections(llmqParams, pindexNew)`: the registry ops `mnconn::check`
/// returns, applied in its order, with each relay set's QSENDRECSIGS sent as
/// `SetMasternodeQuorumRelayMembers` sends it, and each line printed where Core prints it among
/// them. `Role::None` changes nothing. V's reads (the scan, then the ancestors) come before and
/// between the registry's calls, each of which takes and drops its lock inside the call; no
/// NetCore lock is taken until the ops are applied, after the last V read.
///
/// Never called on V's thread. May make bounded reads of V (Core takes cs_main briefly inside
/// it); holds no NetCore or leaf lock across one.
// Core: llmq/net_quorum.cpp:438-497, :766-835
impl QuorumConnectionsCheck for NodeQuorumConnections {
    fn check(&self, llmq_type: LlmqType, params: &LlmqParams, tip: &TipRef, role: &Role) {
        let parts = &self.0;
        if *role == Role::None {
            return;
        }
        let Some(scan) =
            parts.chain.scan(llmq_type, &tip.hash, usize::from(params.keep_old_connections))
        else {
            return;
        };
        let tip_list = parts.tip_list.load();
        let ancestor = |height| parts.chain.ancestor(&tip.hash, height).unwrap_or([0; 32]);
        let quorums = |llmq_type| parts.registry.quorums(llmq_type);
        let has = |llmq_type, hash: &[u8; 32]| parts.registry.has(llmq_type, hash);
        let ctx = CheckCtx {
            llmq_type,
            params,
            tip: QuorumTip {
                height: tip.height,
                hash: *tip.hash.as_bytes(),
            },
            ancestor: &ancestor,
            rotation_enabled: scan.rotation_enabled,
            dip24_is_type: parts.params.consensus.llmq_type_dip0024_instant_send,
            all_members_connected: all_members_connected(llmq_type, &parts.sporks),
            topology: &LlmqTopology,
            watch_seed: &parts.watch_seed,
            registry: RegistryView {
                quorums: &quorums,
                has: &has,
            },
            tip_list: &tip_list,
        };
        let Check {
            ops,
            log,
            op_at,
        } = check(&ctx, mnconn_role(role), scan.quorums);
        debug_assert_eq!(ops.len(), op_at.len());
        let mut printed = 0;
        for (op, at) in ops.into_iter().zip(op_at) {
            emit(&log[printed..at]);
            printed = at;
            self.apply(op);
        }
        emit(&log[printed..]);
    }
}

/// mnconn's role for `NetQuorum`'s `m_role`.
fn mnconn_role(role: &Role) -> mnconn::Role {
    match *role {
        Role::None => mnconn::Role::None,
        Role::Masternode {
            pro_tx,
            watching,
        } => mnconn::Role::Masternode {
            pro_tx_hash: pro_tx.map(|p| *p.as_bytes()),
            watching,
        },
        Role::Observer => mnconn::Role::Observer,
    }
}

/// One load of the published tip list and one read of the members at `quorum_base`, both
/// before any registry or NetCore call, then `EnsureQuorumConnections` as a masternode, then,
/// when `we_are_member`, `AddQuorumProbeConnections`, both over that list.
///
/// Never called on V's thread. May make bounded reads of V (Core takes cs_main briefly inside
/// it); holds no NetCore or leaf lock across one.
// Core: llmq/net_dkg.cpp:880-888 at v24.0.0
impl DkgConnections for NodeQuorumConnections {
    fn on_initialized(
        &self,
        llmq_type: LlmqType,
        params: &LlmqParams,
        quorum_base: &TipRef,
        my_pro_tx: &ProTxHash,
        watch: bool,
        we_are_member: bool,
    ) {
        let parts = &self.0;
        let tip_list = parts.tip_list.load();
        let Some(members) = parts.chain.members(llmq_type, &quorum_base.hash) else {
            return;
        };
        let quorum = QuorumInfo {
            llmq_type,
            quorum_hash: *quorum_base.hash.as_bytes(),
            base_hash: *quorum_base.hash.as_bytes(),
            base_height: quorum_base.height,
            members,
        };
        self.ensure_dkg(params, &quorum, &tip_list, my_pro_tx, watch);
        if we_are_member {
            self.add_probes(&quorum, &tip_list, my_pro_tx);
        }
    }
}

/// mnconn's lines under Core's categories: `EnsureQuorumConnections`' and
/// `AddQuorumProbeConnections`' under `net|netconn`, each member block as one message and only
/// when `llmq` debug is on, the rest under `llmq`.
// Core: llmq/net_quorum.cpp:786-787, :814-827; :425, :431, :458, :473, :483, :493;
// llmq/net_dkg.cpp:828-840
fn emit(lines: &[String]) {
    let mut lines = lines.iter().peekable();
    while let Some(line) = lines.next() {
        if !line.starts_with("EnsureQuorumConnections -- ")
            && !line.starts_with("AddQuorumProbeConnections -- ")
        {
            tracing::debug!(target: "llmq", "{line}");
            continue;
        }
        let block = line.contains(" -- adding masternodes ");
        let mut message = line.clone();
        while let Some(member) = lines.next_if(|l| l.starts_with("  ")) {
            message.push('\n');
            message.push_str(member);
        }
        if block && !tracing::enabled!(target: "llmq", tracing::Level::DEBUG) {
            continue;
        }
        tracing::debug!(target: "net|netconn", "{message}");
    }
}
