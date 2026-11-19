//! The network half of Core's MNAUTH intake.

use std::sync::Arc;

use dash_evo::types::ProTxHash;
use dash_p2p::policy::Throw;
use dash_p2p::{CommandString, PeerHandle, Why};
use dash_p2p_core::{MnAuth, QSendRecSigs};
use dashd_p2p::processing::logic::out_msg;
use dashd_p2p::processing::registry::Extension;
use dashd_p2p::{HandlerCx, MessageProcessingResult, NetHandler};

use super::decide::{Decision, Effect, Input, OtherPeer, PeerFacts, decide};
use crate::active::ActiveMasternode;
use crate::meta::MasternodeMetaMan;

const COMMANDS: [CommandString; 1] = [CommandString::MNAUTH];

/// `CConnman::IsMasternodeQuorumRelayMember(proTxHash)`.
// Core: net.cpp:4494-4506 at v24.0.0
pub type RelayMember = Arc<dyn Fn(&[u8; 32]) -> bool + Send + Sync>;

/// Core's `CMNAuth::ProcessMessage`.
pub struct MnAuthHandler {
    meta: Arc<MasternodeMetaMan>,
    active: Option<Arc<ActiveMasternode>>,
    relay_member: RelayMember,
}

fn verified(hash: [u8; 32]) -> Option<[u8; 32]> {
    (hash != [0; 32]).then_some(hash)
}

impl MnAuthHandler {
    /// The handler of a node with `active` as its masternode manager, if any. No proTxHash is a
    /// quorum relay member until [`MnAuthHandler::with_relay_member`] says otherwise.
    pub fn new(meta: Arc<MasternodeMetaMan>, active: Option<Arc<ActiveMasternode>>) -> Self {
        Self {
            meta,
            active,
            relay_member: Arc::new(|_| false),
        }
    }

    /// Answers `IsMasternodeQuorumRelayMember` with `relay_member`.
    pub fn with_relay_member(mut self, relay_member: RelayMember) -> Self {
        self.relay_member = relay_member;
        self
    }

    fn peer_facts(peer: &PeerHandle) -> PeerFacts {
        PeerFacts {
            id: peer.id(),
            verified: verified(peer.verified_proreg_hash()),
            services: peer.version_services(),
            inbound: peer.is_inbound(),
            probe: peer.masternode_probe(),
            sent_challenge: peer.sent_mnauth_challenge(),
            version: peer.version(),
            disconnect: peer.disconnect_requested(),
        }
    }

    // Core: net.h:1432; net.cpp:4874 at v24.0.0
    fn others(cx: &HandlerCx<'_>) -> Vec<OtherPeer> {
        cx.core
            .peers()
            .iter()
            .filter(|p| p.successfully_connected() && !p.disconnect_requested())
            .map(|p| OtherPeer {
                id: p.id(),
                verified: verified(p.verified_proreg_hash()),
                inbound: p.is_inbound(),
            })
            .collect()
    }

    fn apply(&self, cx: &mut HandlerCx<'_>, d: &Decision) {
        for line in &d.log {
            tracing::debug!(target: "net|netconn", "{line}");
        }
        for effect in &d.effects {
            match *effect {
                Effect::SetLastOutboundSuccess {
                    pro,
                    now,
                } => self.meta.set_last_outbound_success(&ProTxHash::from(pro), now),
                Effect::MarkProbe(id) => {
                    if let Some(peer) = cx.core.peer(id) {
                        peer.set_masternode_probe(true);
                    }
                }
                Effect::Disconnect(id) => cx.core.disconnect(id, Why::Processor("mnauth")),
            }
        }
        if let Some((score, reason)) = d.score {
            cx.post_process(MessageProcessingResult::misbehaving(score as i32, reason));
        }
    }
}

impl NetHandler for MnAuthHandler {
    fn commands(&self) -> &'static [CommandString] {
        &COMMANDS
    }

    fn extension(&self) -> Option<Extension> {
        Some(Extension::MnAuth)
    }

    // Core: evo/mnauth.cpp:51-177 at v24.0.0
    fn process_message(
        &mut self,
        cx: &mut HandlerCx<'_>,
        cmd: &CommandString,
        payload: &[u8],
    ) -> Result<(), Throw> {
        // Core: evo/mnauth.cpp:57-60 at v24.0.0
        if *cmd != CommandString::MNAUTH || !cx.mn_net.blockchain_synced() {
            return Ok(());
        }
        let msg: MnAuth = cx.read(&mut &payload[..])?;
        let pro: [u8; 32] = msg.pro_reg_tx_hash().into();
        let tip = cx.mn_net.tip_list();
        let op = tip.operator(&pro);
        let input = Input {
            msg,
            peer: Self::peer_facts(cx.peer),
            op,
            bls_legacy: tip.bls_legacy_scheme(),
            me: self.active.as_ref().and_then(|a| a.pro_tx_hash()),
            others: Self::others(cx),
            now: cx.now_us.div_euclid(1_000_000),
        };
        // Core: bls/bls.h:196 at v24.0.0
        let d = decide(input).map_err(|_| Throw::ios_failure("malleable BLS object"))?;
        self.apply(cx, &d);
        // Core: evo/mnauth.cpp:154-156 at v24.0.0
        if !d.grant || cx.peer.disconnect_requested() {
            return Ok(());
        }
        let Some(op) = op else {
            unreachable!("a granted MNAUTH has a masternode entry");
        };
        // Core: evo/mnauth.cpp:158-163 at v24.0.0
        if !cx.core.try_mark_verified(cx.id(), pro, op.pubkey_hash) {
            cx.disconnect("mnauth verified inbound cap");
            return Ok(());
        }
        // Core: evo/mnauth.cpp:165-173 at v24.0.0
        if !cx.peer.masternode_iqr() && (self.relay_member)(&pro) {
            cx.push(out_msg(CommandString::QSENDRECSIGS, &QSendRecSigs::new(true)));
            cx.peer.set_masternode_iqr(true);
        }
        // Core: evo/mnauth.cpp:175 at v24.0.0
        tracing::debug!(
            target: "net|netconn",
            "CMNAuth::ProcessMessage -- Valid MNAUTH for {}, peer={}",
            ProTxHash::from(pro),
            cx.id()
        );
        Ok(())
    }
}
