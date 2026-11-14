use std::sync::Arc;

use dash_evo::bls::BlsCtx;
pub use dash_evo::locks::ChainLockSig;
use dash_p2p_core::ClSig;
use dashd_p2p::processing::registry::MessageProcessingResult;
pub use dashd_p2p::processing::seams::Read;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClVerdict {
    Valid,
    NoQuorum,
    Invalid,
    Unavailable(String),
    Corrupt(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stopped;

#[derive(Clone, Debug, Default)]
pub struct ClSigsSnapshot {
    pub best: Option<Arc<ChainLockSig>>,
    pub best_hash: [u8; 32],
}

impl ClSigsSnapshot {
    pub fn by_hash(&self, hash: &[u8; 32], write: BlsCtx) -> Option<ClSig> {
        (self.best_hash == *hash).then(|| {
            self.best
                .as_ref()
                .map_or_else(|| ChainLockSig::null().to_wire(write), |sig| sig.to_wire(write))
        })
    }
}

pub trait ClChain: Send {
    /// Runs the active-state check and publishes its result before returning the best height.
    fn intake(&self) -> Result<i32, Stopped>;
    fn record(
        &self,
        sig: Arc<ChainLockSig>,
        sig_hash: &[u8; 32],
        from: i64,
    ) -> Result<bool, Stopped>;
    fn reads(&self) -> ClSigsSnapshot;
    fn abort_node(&self, msg: String);
}

pub trait ClVerify: Send {
    fn verify(&self, read: &Read<ClSig>) -> ClVerdict;
}

pub trait ClRelay {
    fn post(&self, result: MessageProcessingResult);
}
