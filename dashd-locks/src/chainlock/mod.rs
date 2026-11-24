use std::sync::Arc;

use dash_clock::Clock;
use dash_evo::bls::BlsCtx;
use dash_num::Hash256;
use dash_p2p_core::{ClSig, InvType, Inventory};
use dash_types::Numeric;
use dashd_p2p::processing::chainlock_seen::{SeenChainLocksCleanup, SeenChainLocksMap};
use dashd_p2p::processing::registry::MessageProcessingResult;
use dashd_p2p::processing::seams::ChainLocks;

pub use self::traits::{
    ChainLockSig, ClChain, ClRelay, ClSigsSnapshot, ClVerdict, ClVerify, Read, Stopped,
};

pub mod coinbase;
#[cfg(test)]
mod fakes;
pub mod first_seen;
pub mod reads;
pub mod seen;
pub mod signer;
pub mod traits;

pub struct ChainLockHandler<C: ClChain, V: ClVerify> {
    chain: C,
    verify: V,
    seen: SeenChainLocksMap,
    enabled: Box<dyn Fn() -> bool + Send>,
}

impl<C: ClChain, V: ClVerify> ChainLockHandler<C, V> {
    pub fn new(
        chain: C,
        verify: V,
        clock: Arc<dyn Clock>,
        enabled: impl Fn() -> bool + Send + 'static,
    ) -> Self {
        Self {
            chain,
            verify,
            seen: SeenChainLocksMap::new(clock),
            enabled: Box::new(enabled),
        }
    }

    pub fn cleanup(&self) -> SeenChainLocksCleanup {
        self.seen.cleanup()
    }

    pub fn seen_len(&self) -> usize {
        self.seen.len()
    }

    pub fn verifier(&self) -> &V {
        &self.verify
    }

    /// The coinbase route's entry: the caller samples `Read` at processing time and uses no peer
    /// (`from = -1`); the result goes to `relay`, as Core's `PostProcessMessage(.., -1)`.
    // Core: net_processing.cpp:2167-2169 at v24.0.0
    pub fn process_external(&mut self, read: Read<ClSig>, hash: &[u8; 32], relay: &impl ClRelay) {
        let _ = self.try_process_external(read, hash, relay);
    }

    /// [`Self::process_external`], with `Err(Stopped)` when `intake` or `record` found V
    /// stopped: the lock was then not processed to its end, and `relay` heard nothing.
    pub fn try_process_external(
        &mut self,
        read: Read<ClSig>,
        hash: &[u8; 32],
        relay: &impl ClRelay,
    ) -> Result<(), Stopped> {
        relay.post(self.process(-1, read, hash)?);
        Ok(())
    }

    // Core: chainlock/handler.cpp:133-183 ProcessNewChainLock at v24.0.0
    fn process(
        &mut self,
        from: i64,
        read: Read<ClSig>,
        hash: &[u8; 32],
    ) -> Result<MessageProcessingResult, Stopped> {
        let best_height = self.chain.intake()?;
        if !self.seen.insert(*hash) || read.msg.height() <= best_height {
            return Ok(MessageProcessingResult::default());
        }
        match self.verify.verify(&read) {
            ClVerdict::Valid => {}
            ClVerdict::Unavailable(error) => {
                tracing::warn!(target: "chainlocks", %error, from, "ChainLock verification unavailable");
                return Ok(MessageProcessingResult::default());
            }
            ClVerdict::Corrupt(error) => {
                self.chain.abort_node(error);
                return Ok(MessageProcessingResult::default());
            }
            ClVerdict::NoQuorum | ClVerdict::Invalid => {
                return Ok(if from == -1 {
                    MessageProcessingResult::default()
                } else {
                    MessageProcessingResult::misbehaving(10, "")
                });
            }
        }
        let sig = match ChainLockSig::from_wire(&read.msg, read.read) {
            Ok(sig) => Arc::new(sig),
            Err(error) => {
                self.chain.abort_node(format!("verified ChainLock could not be decoded: {error}"));
                return Ok(MessageProcessingResult::default());
            }
        };
        Ok(if self.chain.record(sig, hash, from)? {
            MessageProcessingResult::relay(Inventory::new(
                InvType::ClSig,
                Hash256::from_lendian(*hash),
            ))
        } else {
            MessageProcessingResult::default()
        })
    }
}

impl<C: ClChain, V: ClVerify> ChainLocks for ChainLockHandler<C, V> {
    fn is_enabled(&self) -> bool {
        (self.enabled)()
    }

    // Core: chainlock/handler.cpp:76 AlreadyHave at v24.0.0
    fn already_have(&self, hash: &[u8; 32]) -> bool {
        self.seen.contains(hash) || self.chain.reads().best_hash == *hash
    }

    // Core: chainlock/handler.cpp:133 ProcessNewChainLock at v24.0.0
    fn process_new_chainlock(
        &mut self,
        from: i64,
        read: Read<ClSig>,
        hash: &[u8; 32],
    ) -> MessageProcessingResult {
        self.process(from, read, hash).unwrap_or_default()
    }

    fn chainlock_by_hash(&self, hash: &[u8; 32], write: BlsCtx) -> Option<ClSig> {
        self.chain.reads().by_hash(hash, write)
    }

    fn best_chainlock(&self, write: BlsCtx) -> Option<ClSig> {
        self.chain.reads().best.as_ref().map(|sig| sig.to_wire(write))
    }
}
