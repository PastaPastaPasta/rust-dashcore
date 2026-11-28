//! The facts and effects the sig-share runtime reads and applies, sampled per call.

use dash_llmq::dkg::bls::BlsCtx;
use dash_llmq::sigenv::{Now, PeerId, SigMsg, SignerFacts, SporkFacts};
use rand_core::Rng;

/// Node facts, each sampled when an entry point runs.
pub trait SharesFacts: Send + Sync {
    type Rng: Rng;
    fn signer(&self) -> SignerFacts;
    fn sporks(&self) -> SporkFacts;
    fn now(&self) -> Now;
    /// The loaded view's BLS context, Core's global scheme at the call.
    fn bls(&self) -> BlsCtx;
    fn rng(&self) -> Self::Rng;
}

/// The peer manager calls the runtime makes.
pub trait SharesOut: Send + Sync {
    /// Pushes `msgs` to `peer` in order.
    // Core: v24.0.0 src/llmq/signing_shares.cpp:1201-1297 CSigSharesManager::SendMessages
    fn send(&self, peer: PeerId, msgs: Vec<SigMsg>);
    /// Core's `PeerMisbehaving`.
    fn misbehaving(&self, peer: PeerId, score: i32);
}
