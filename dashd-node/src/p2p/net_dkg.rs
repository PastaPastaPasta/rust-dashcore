//! DK1-L3: an active masternode's `NetDKG` over ND1's intake ([`DkgIntake`]). Step 7c registers
//! the handler before the processor is built, and installs the intake into its [`NetDkgSlot`]
//! once the DKG sessions are built, still inside step 7c and before any peer is processed.
// Core: v24.0.0 src/init.cpp:2214-2220; src/llmq/net_dkg.cpp:336-368, :505-748

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, OnceLock};

use dash_evo::types::ProTxHash;
use dash_llmq::dkg::session::{DkgInv, DkgMsgKind};
use dash_llmq::qdata::PeerId;
use dash_num::Hash256;
use dash_p2p::policy::Throw;
use dash_p2p::{CommandString, NetCore, OutMsg};
use dash_p2p_core::{InvType, Inventory};
use dash_params::LlmqType;
use dash_primitives::QuorumHash;
use dash_types::Numeric as _;
use dashd_chainstate::chain::ChainHandle;
use dashd_masternode::mnconn::QuorumNodes;
use dashd_p2p::processing::peerman::Shared;
use dashd_p2p::processing::registry::CoreHandler;
use dashd_p2p::processing::solicit;
use dashd_p2p::{HandlerCx, NetHandler};
use dashd_quorums::dkg::intake::{DkgIntake, DkgNetMsg, Verdict};
use dashd_quorums::dkg::seams::{BaseFacts, GetDataResponse, IntakeHost, RelayFacts, RelayPeer};

const KINDS: [(CommandString, DkgMsgKind, InvType); 4] = [
    (CommandString::QCONTRIB, DkgMsgKind::Contribution, InvType::QuorumContrib),
    (CommandString::QCOMPLAINT, DkgMsgKind::Complaint, InvType::QuorumComplaint),
    (CommandString::QJUSTIFY, DkgMsgKind::Justification, InvType::QuorumJustification),
    (CommandString::QPCOMMIT, DkgMsgKind::PrematureCommitment, InvType::QuorumPrematureCommitment),
];

const COMMANDS: [CommandString; 5] = [
    CommandString::QCONTRIB,
    CommandString::QCOMPLAINT,
    CommandString::QJUSTIFY,
    CommandString::QPCOMMIT,
    CommandString::QWATCH,
];

fn dkg_inv(inv: &Inventory) -> Option<DkgInv> {
    let (_, kind, _) = KINDS.iter().find(|(_, _, t)| *t == inv.inv_type())?;
    Some(DkgInv {
        kind: *kind,
        hash: inv.inv_hash().to_lendian(),
    })
}

fn inventory(inv: DkgInv) -> Option<Inventory> {
    let (_, _, t) = KINDS.iter().find(|(_, k, _)| *k == inv.kind)?;
    Some(Inventory::new(*t, Hash256::from_lendian(inv.hash)))
}

/// A failed local read inside `ProcessMessage`: Core's `ProcessMessages` catches the store's
/// `dbwrapper_error`, logs it and keeps the peer unscored (DEC-1881). Its `what` carries the
/// read's error, as `dbwrapper_error`'s carries the store's status.
// Core: v24.0.0 src/net_processing.cpp:5945-5962 ProcessMessages; src/dbwrapper.h:46
fn read_failed(error: &str) -> Throw {
    Throw {
        what: format!("NetDKG: the IsQuorumTypeEnabled read failed: {error}").into(),
        type_name: "15dbwrapper_error",
    }
}

fn verified(hash: [u8; 32]) -> Option<ProTxHash> {
    (hash != [0; 32]).then(|| ProTxHash::from(hash))
}

/// The peers that sent `qwatch`: Core's `CNode::qwatch`.
#[derive(Default)]
pub struct QwatchPeers(Mutex<BTreeSet<PeerId>>);

impl QwatchPeers {
    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeSet<PeerId>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The intake's reads and effects from any thread: the phase threads' scores and relays, and
/// the base check's chain read.
pub struct NodeIntakeHost {
    pub core: Arc<NetCore>,
    pub shared: Arc<Shared>,
    pub registry: Arc<QuorumNodes>,
    pub chain: ChainHandle,
    pub qwatch: Arc<QwatchPeers>,
}

impl IntakeHost for NodeIntakeHost {
    fn verified_pro_tx(&self, peer: PeerId) -> Option<ProTxHash> {
        verified(self.core.peer(peer)?.verified_proreg_hash())
    }

    /// Never asked: the message thread's own host answers for the message's peer, whose
    /// state it holds.
    fn consume_getdata_response(&self, _: PeerId, _: DkgInv) -> GetDataResponse {
        GetDataResponse::Unrequested
    }

    /// `NodeIntakeHost::read_quorum_type_enabled`, `None` for its error.
    fn quorum_type_enabled(&self, t: LlmqType, base: &BaseFacts) -> Option<bool> {
        self.read_quorum_type_enabled(t, base).ok()
    }

    // Core: v24.0.0 src/net_processing.cpp:6919-6923 PeerMisbehaving
    fn misbehaving(&self, peer: PeerId, score: i32) {
        self.shared.misbehaving(peer, score, "");
    }

    /// `GetNodeCount(Both)` skips the peers being disconnected, and `ForEachNode` visits the
    /// fully connected ones in table order. A `qwatch` mark of a peer no longer in the table is
    /// dropped here.
    // Core: v24.0.0 src/net.cpp:4413-4417, :4520-4540; src/net.h:1353-1359, :1405-1411
    fn relay_facts(&self, t: LlmqType, base: &QuorumHash) -> RelayFacts {
        let peers = self.core.peers();
        let has_quorum_nodes = self.registry.has(t, base.as_bytes());
        let mut qwatch = self.qwatch.lock();
        qwatch.retain(|id| peers.iter().any(|p| p.id() == *id));
        RelayFacts {
            node_count: peers.iter().filter(|p| !p.disconnect_requested()).count(),
            network_active: self.core.network_active(),
            has_quorum_nodes,
            peers: peers
                .iter()
                .filter(|p| p.successfully_connected() && !p.disconnect_requested())
                .map(|p| RelayPeer {
                    id: p.id(),
                    addr_name: p.addr_name().to_owned(),
                    verified_pro_tx: verified(p.verified_proreg_hash()),
                    qwatch: qwatch.contains(&p.id()),
                })
                .collect(),
        }
    }

    // Core: v24.0.0 src/net_processing.cpp:6979-6982 PeerPushInventory
    fn push_inventory(&self, peer: PeerId, inv: DkgInv) {
        if let Some(inv) = inventory(inv) {
            self.shared.push_inventory(peer, &inv);
        }
    }
}

impl NodeIntakeHost {
    /// Route B at the base's parent; the error once V has stopped, for a base or parent the
    /// index does not hold, or when the read fails.
    // Core: v24.0.0 src/llmq/net_dkg.cpp:587, src/validation.cpp:6678
    fn read_quorum_type_enabled(&self, t: LlmqType, base: &BaseFacts) -> Result<bool, String> {
        let (hash, height) = (base.hash, base.height);
        self.chain
            .on_v(move |on_v, q| {
                let reader = q.chain.reader();
                let parent = height.checked_sub(1).and_then(|h| u32::try_from(h).ok());
                let prev = reader
                    .lookup(&hash)
                    .zip(parent)
                    .and_then(|(id, parent)| reader.ancestor(id, parent))
                    .ok_or_else(|| format!("no index entry for the parent of {hash}"))?;
                q.bind(on_v).quorum_type_enabled(t, prev).map_err(|e| e.to_string())
            })
            .recv()
            .map_err(|_| "the chainstate has stopped".to_owned())?
    }
}

/// One message's host: the message thread's context for the sender's MNAuth hash and its
/// `getdata` record, the node's host for the rest, and the base check's read error, if any.
struct CxHost<'h, 'c> {
    cx: Mutex<&'h mut HandlerCx<'c>>,
    node: &'h NodeIntakeHost,
    read_error: Mutex<Option<String>>,
}

impl IntakeHost for CxHost<'_, '_> {
    fn verified_pro_tx(&self, _: PeerId) -> Option<ProTxHash> {
        verified(self.cx.lock().unwrap_or_else(|e| e.into_inner()).peer.verified_proreg_hash())
    }

    // Core: v24.0.0 src/net_processing.cpp:6943-6972 PeerConsumeGetDataResponse
    fn consume_getdata_response(&self, _: PeerId, inv: DkgInv) -> GetDataResponse {
        let Some(inv) = inventory(inv) else {
            return GetDataResponse::Unrequested;
        };
        match self.cx.lock().unwrap_or_else(|e| e.into_inner()).consume_getdata_response(&inv) {
            solicit::GetDataResponse::Requested => GetDataResponse::Requested,
            solicit::GetDataResponse::Late => GetDataResponse::Late,
            solicit::GetDataResponse::Unrequested => GetDataResponse::Unrequested,
        }
    }

    fn quorum_type_enabled(&self, t: LlmqType, base: &BaseFacts) -> Option<bool> {
        self.node
            .read_quorum_type_enabled(t, base)
            .map_err(|e| *self.read_error.lock().unwrap_or_else(|p| p.into_inner()) = Some(e))
            .ok()
    }

    fn misbehaving(&self, peer: PeerId, score: i32) {
        self.node.misbehaving(peer, score);
    }

    fn relay_facts(&self, t: LlmqType, base: &QuorumHash) -> RelayFacts {
        self.node.relay_facts(t, base)
    }

    fn push_inventory(&self, peer: PeerId, inv: DkgInv) {
        self.node.push_inventory(peer, inv);
    }
}

/// The intake step 7c installs once the sessions are built.
pub struct Installed {
    pub intake: Arc<DkgIntake>,
    pub host: Arc<NodeIntakeHost>,
}

/// Where the masternode's `NetDKG` finds its intake, and the `qwatch` marks it keeps.
#[derive(Default)]
pub struct NetDkgSlot {
    installed: OnceLock<Installed>,
    pub qwatch: Arc<QwatchPeers>,
}

impl NetDkgSlot {
    /// Installs the intake; a second install is a wiring bug.
    ///
    /// # Panics
    ///
    /// When an intake is already installed.
    pub fn install(&self, installed: Installed) {
        assert!(self.installed.set(installed).is_ok(), "the DKG intake is installed once");
    }
}

/// An active masternode's `NetDKG`: `ProcessMessage`, `AlreadyHave` and `ProcessGetData` over
/// the installed intake.
pub struct MnNetDkg(pub Arc<NetDkgSlot>);

impl NetHandler for MnNetDkg {
    fn commands(&self) -> &'static [CommandString] {
        &COMMANDS
    }

    fn core_handler(&self) -> Option<CoreHandler> {
        Some(CoreHandler::NetDkg)
    }

    // Core: v24.0.0 src/llmq/net_dkg.cpp:505-679 NetDKG::ProcessMessage
    fn process_message(
        &mut self,
        cx: &mut HandlerCx<'_>,
        cmd: &CommandString,
        payload: &[u8],
    ) -> Result<(), Throw> {
        let Some(installed) = self.0.installed.get() else {
            return Ok(());
        };
        let msg = match KINDS.iter().find(|(c, _, _)| c == cmd) {
            Some((_, kind, _)) => DkgNetMsg::Dkg(*kind),
            None if *cmd == CommandString::QWATCH => DkgNetMsg::Qwatch,
            None => return Ok(()),
        };
        let peer = cx.id();
        let (verdict, read_error) = {
            let host = CxHost {
                cx: Mutex::new(&mut *cx),
                node: &installed.host,
                read_error: Mutex::default(),
            };
            let verdict = installed.intake.process_message_with(&host, peer, msg, payload.to_vec());
            (verdict, host.read_error.into_inner().unwrap_or_else(|p| p.into_inner()))
        };
        match verdict {
            Verdict::SetQwatch => {
                self.0.qwatch.lock().insert(peer);
            }
            Verdict::Misbehaving {
                score,
                message,
            } => cx.misbehaving(score, message),
            Verdict::ReadFailed => {
                return Err(read_failed(read_error.as_deref().unwrap_or("no error recorded")));
            }
            Verdict::Ignored | Verdict::Dispatched => {}
        }
        Ok(())
    }

    // Core: v24.0.0 src/llmq/net_dkg.cpp:681-704 NetDKG::AlreadyHave
    fn already_have(&self, inv: &Inventory) -> bool {
        let Some(inv) = dkg_inv(inv) else {
            return false;
        };
        self.0.installed.get().is_none_or(|i| i.intake.already_have(inv))
    }

    /// Serialized under the message's BLS scheme, Core's `bls::bls_legacy_scheme` at the call.
    // Core: v24.0.0 src/llmq/net_dkg.cpp:706-748 NetDKG::ProcessGetData
    fn process_getdata(&mut self, cx: &mut HandlerCx<'_>, inv: &Inventory) -> bool {
        let (Some(installed), Some(inv)) = (self.0.installed.get(), dkg_inv(inv)) else {
            return false;
        };
        let legacy = cx.ctx.bls().scheme().is_legacy();
        let Some((kind, payload)) = installed.intake.process_getdata(inv, legacy) else {
            return false;
        };
        let Some((cmd, _, _)) = KINDS.iter().find(|(_, k, _)| *k == kind) else {
            return false;
        };
        cx.push(OutMsg::new(*cmd, payload));
        true
    }
}
