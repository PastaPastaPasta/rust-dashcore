//! Core's `CSigSharesManager` (`ActiveContext::shareman`) over the node: the facts it samples,
//! the operator key, its peer-manager calls and the processor's [`SigSharesSeam`].

use std::convert::Infallible;
use std::sync::{Arc, OnceLock, Weak};

use dash_clock::Clock;
use dash_evo::spork::SporkId;
use dash_evo::types::ProTxHash;
use dash_llmq::dkg::bls::{BlsCtx, BlsSchemeKind, BlsSigValue};
use dash_llmq::sigenv::{
    Now, OperatorSigner, PeerFacts, PeerId, QuorumLookup, RecSigReads, ShareRecovery, SigMsg,
    SigSharesNet, SignerFacts, SporkFacts,
};
use dash_llmq::signing::api::{RecoveredSigIntake, SigningFault};
use dash_p2p::rng::{FastRandom, RandSource, process_source};
use dash_p2p::{CommandString, NetCore};
use dash_p2p_core::{
    BatchedSigShares, BatchedSigSharesMsg, SigSesAnn, SigSesAnnMsg, SigShare, SigShareMsg,
    SigSharesInv, SigSharesInvMsg,
};
use dash_params::LlmqType;
use dash_primitives::QuorumHash;
use dashd_masternode::active::ActiveMasternode;
use dashd_masternode::mnconn::QuorumNodes;
use dashd_masternode::sporks::SporkManager;
use dashd_p2p::Shared;
use dashd_p2p::processing::eviction::fully_connected;
use dashd_p2p::processing::logic::out_msg;
use dashd_p2p::processing::seams::{SigSharesInvKind, SigSharesSeam};
use dashd_quorums::LocalMasternode;
use dashd_quorums::signing::SignQueue;
use dashd_quorums::sigshares::{SharesFacts, SharesInvKind, SharesOut, SigSharesRuntime};

use crate::init::signing::SigningRng;

#[cfg(test)]
mod tests;

/// The active masternode's identity as the signing runtime reads it.
pub struct ActiveIdentity(pub Arc<ActiveMasternode>);

impl LocalMasternode for ActiveIdentity {
    // Core: v24.0.2 src/llmq/signing_shares.cpp:803
    fn pro_tx_hash(&self) -> Option<ProTxHash> {
        self.0.pro_tx_hash().map(ProTxHash::from)
    }
}

/// `CreateSigShareForSingleMember`'s `m_mn_activeman.Sign`.
pub struct ActiveOperator(pub Arc<ActiveMasternode>);

impl OperatorSigner for ActiveOperator {
    // Core: v24.0.2 src/llmq/signing_shares.cpp:1580-1582
    fn sign(&self, hash: &[u8; 32], scheme: BlsSchemeKind) -> Option<BlsSigValue> {
        Some(self.0.sign(hash, scheme == BlsSchemeKind::Legacy))
    }
}

/// What the share runtime samples per call.
pub struct NodeShareFacts {
    pub active: Arc<ActiveMasternode>,
    pub sporks: Arc<SporkManager>,
    pub clock: Arc<dyn Clock>,
    /// Core's `bls::bls_legacy_scheme`: V's published scheme.
    pub legacy: Box<dyn Fn() -> bool + Send + Sync>,
}

impl SharesFacts for NodeShareFacts {
    type Rng = SigningRng;

    /// rdashd has no assumeutxo snapshot, so `IsQuorumSigningAllowed` always holds.
    // Core: v24.0.2 src/llmq/signing_shares.cpp:1561-1564
    fn signer(&self) -> SignerFacts {
        SignerFacts {
            my_pro_tx: ActiveIdentity(Arc::clone(&self.active)).pro_tx_hash(),
            signing_allowed: true,
        }
    }

    fn sporks(&self) -> SporkFacts {
        SporkFacts {
            spork21_active: self.sporks.is_active(SporkId::SPORK_21_QUORUM_ALL_CONNECTED),
            spork21_value: self.sporks.value(SporkId::SPORK_21_QUORUM_ALL_CONNECTED),
        }
    }

    fn now(&self) -> Now {
        Now {
            ms: self.clock.node_now_micros().div_euclid(1000),
        }
    }

    fn bls(&self) -> BlsCtx {
        BlsCtx::from_legacy((self.legacy)())
    }

    fn rng(&self) -> SigningRng {
        SigningRng(FastRandom::new())
    }
}

/// `GetStrongRandBytes` as the share verifier's weights.
pub struct StrongRng(Arc<dyn RandSource>);

impl Default for StrongRng {
    fn default() -> Self {
        Self(process_source())
    }
}

impl rand_core::TryRng for StrongRng {
    type Error = Infallible;

    fn try_next_u32(&mut self) -> Result<u32, Infallible> {
        let mut b = [0; 4];
        self.0.rand_bytes(&mut b, true);
        Ok(u32::from_le_bytes(b))
    }

    fn try_next_u64(&mut self) -> Result<u64, Infallible> {
        let mut b = [0; 8];
        self.0.rand_bytes(&mut b, true);
        Ok(u64::from_le_bytes(b))
    }

    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Infallible> {
        self.0.rand_bytes(dst, true);
        Ok(())
    }
}

impl rand_core::TryCryptoRng for StrongRng {}

/// The share manager's `CConnman` and `PeerManager` calls.
pub struct SharesNet {
    pub shared: Arc<Shared>,
    pub core: Arc<NetCore>,
    pub quorums: Arc<QuorumNodes>,
}

impl SigSharesNet for SharesNet {
    /// The registry is filled by the quorum-connection tip listener (`SetMasternodeQuorumNodes`).
    // Core: v24.0.2 src/net.cpp:4433-4456 CConnman::GetMasternodeQuorumNodes,
    // src/llmq/net_quorum.cpp:376-401
    fn quorum_nodes(&self, t: LlmqType, h: &QuorumHash) -> Vec<PeerId> {
        self.quorums.quorum_nodes(t, h.as_bytes(), &self.core.peers())
    }

    // Core: v24.0.2 src/net.cpp:4875-4878 CConnman::NodeFullyConnected
    fn fully_connected(&self) -> Vec<PeerFacts> {
        let peers = self.core.peers();
        let connected = peers.iter().filter(|p| fully_connected(p));
        connected
            .map(|p| PeerFacts {
                id: p.id(),
                verified_pro_tx: Some(p.verified_proreg_hash())
                    .filter(|h| *h != [0; 32])
                    .map(ProTxHash::from),
                common_version: p.common_version().into(),
            })
            .collect()
    }

    // Core: v24.0.2 src/net_processing.cpp:6925-6928, :1962-1972 PeerIsBanned, IsBanned
    fn is_banned(&self, p: PeerId) -> bool {
        self.shared.misbehavior(p).is_some_and(|(_, discourage)| discourage)
    }
}

impl SharesOut for SharesNet {
    // Core: v24.0.2 src/llmq/signing_shares.cpp:1144-1297 CSigSharesManager::SendMessages
    fn send(&self, peer: PeerId, msgs: Vec<SigMsg>) {
        for msg in msgs {
            let msg = match msg {
                SigMsg::SesAnn(m) => out_msg(CommandString::QSIGSESANN, &SigSesAnnMsg::new(m)),
                SigMsg::GetSigShares(m) => {
                    out_msg(CommandString::QGETSIGS, &SigSharesInvMsg::new(m))
                }
                SigMsg::BSigShares(m) => {
                    out_msg(CommandString::QBSIGS, &BatchedSigSharesMsg::new(m))
                }
                SigMsg::SigSharesInv(m) => {
                    out_msg(CommandString::QSIGSINV, &SigSharesInvMsg::new(m))
                }
                SigMsg::SigShare(m) => out_msg(CommandString::QSIGSHARE, &SigShareMsg::new(m)),
            };
            if !self.shared.push_to(peer, msg) {
                return;
            }
        }
    }

    // Core: v24.0.2 src/net_processing.cpp:6919-6923 PeerManagerImpl::PeerMisbehaving
    fn misbehaving(&self, peer: PeerId, score: i32) {
        self.shared.misbehaving(peer, score, "");
    }
}

/// `AsyncSignIfMember`'s `shareman`, which the signing manager does not own: after the share
/// manager is dropped, a request is dropped too.
pub struct ShareQueue<R>(pub Weak<R>);

impl<Q, R: SignQueue<Q>> SignQueue<Q> for ShareQueue<R> {
    fn force_reannouncement(&self, q: &Arc<Q>, t: LlmqType, id: &[u8; 32], msg: &[u8; 32]) {
        if let Some(shares) = self.0.upgrade() {
            shares.force_reannouncement(q, t, id, msg);
        }
    }

    fn async_sign(&self, quorum: Arc<Q>, id: [u8; 32], msg: [u8; 32]) {
        if let Some(shares) = self.0.upgrade() {
            shares.async_sign(quorum, id, msg);
        }
    }
}

/// The processor's [`SigSharesSeam`] over the share runtime, called inline on the message
/// thread with the sender's state locked: each entry point may wait on V for its quorums, never
/// on a share thread, and sends only through the connection table.
pub struct P2pSigShares<R>(pub Arc<R>);

impl<L, I, N, X, F, S> SigSharesSeam for P2pSigShares<SigSharesRuntime<L, I, N, X, F, S>>
where
    L: QuorumLookup + Send + Sync,
    L::Quorum: Send + Sync,
    I: RecoveredSigIntake + RecSigReads<Error = SigningFault> + Send + Sync,
    N: SigSharesNet + Send + Sync,
    X: SharesOut,
    F: SharesFacts,
    S: ShareRecovery + Send + Sync,
{
    fn spork21_active(&self) -> bool {
        self.0.spork21_active()
    }

    fn ses_ann(&self, peer: PeerId, msgs: Vec<SigSesAnn>) {
        self.0.on_ses_ann(peer, msgs);
    }

    fn sig_shares_inv(&self, peer: PeerId, kind: SigSharesInvKind, msgs: Vec<SigSharesInv>) {
        let kind = match kind {
            SigSharesInvKind::Inv => SharesInvKind::Announced,
            SigSharesInvKind::GetSigs => SharesInvKind::Requested,
        };
        self.0.on_sig_shares_inv(peer, msgs, kind);
    }

    fn batched(&self, peer: PeerId, msgs: Vec<BatchedSigShares>) {
        self.0.on_batched(peer, msgs);
    }

    fn sig_share(&self, peer: PeerId, msgs: Vec<SigShare>) {
        self.0.on_sig_share(peer, msgs);
    }

    fn ban(&self, peer: PeerId) {
        self.0.ban(peer);
    }
}

/// A masternode's share seam from attach: no share manager's answers until
/// `AddExtraHandler(NetSigning)` binds the runtime's seam.
// Core: v24.0.2 src/init.cpp:2202
#[derive(Default)]
pub struct LateSigShares(OnceLock<Arc<dyn SigSharesSeam>>);

impl LateSigShares {
    pub fn bind(&self, seam: Arc<dyn SigSharesSeam>) {
        assert!(self.0.set(seam).is_ok(), "NetSigning added once");
    }
}

impl SigSharesSeam for LateSigShares {
    fn enabled(&self) -> bool {
        self.0.get().is_some_and(|s| s.enabled())
    }

    fn spork21_active(&self) -> bool {
        self.0.get().is_some_and(|s| s.spork21_active())
    }

    fn ses_ann(&self, peer: PeerId, msgs: Vec<SigSesAnn>) {
        if let Some(s) = self.0.get() {
            s.ses_ann(peer, msgs);
        }
    }

    fn sig_shares_inv(&self, peer: PeerId, kind: SigSharesInvKind, msgs: Vec<SigSharesInv>) {
        if let Some(s) = self.0.get() {
            s.sig_shares_inv(peer, kind, msgs);
        }
    }

    fn batched(&self, peer: PeerId, msgs: Vec<BatchedSigShares>) {
        if let Some(s) = self.0.get() {
            s.batched(peer, msgs);
        }
    }

    fn sig_share(&self, peer: PeerId, msgs: Vec<SigShare>) {
        if let Some(s) = self.0.get() {
            s.sig_share(peer, msgs);
        }
    }

    fn ban(&self, peer: PeerId) {
        if let Some(s) = self.0.get() {
            s.ban(peer);
        }
    }
}
