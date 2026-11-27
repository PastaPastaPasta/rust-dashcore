//! The ChainLock handler over the node's V: its [`ClChain`] on V's class M commands and published
//! view, and the handler shared by the P2P seam and the coinbase worker.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use dash_chain::PeerId;
use dash_evo::bls::BlsCtx;
use dash_num::Hash256;
use dash_p2p_core::ClSig;
use dash_types::Numeric as _;
use dashd_chainstate::chain::ChainHandle;
use dashd_locks::chainlock::{
    ChainLockHandler, ChainLockSig, ClChain, ClRelay, ClSigsSnapshot, Read, Stopped,
};
use dashd_p2p::Shared;
use dashd_p2p::processing::registry::MessageProcessingResult;
use dashd_p2p::processing::seams::ChainLocks;

use super::NodeClVerify;
use crate::abort::AbortSink;

/// The node's ChainLock handler: Core's one `ChainlockHandler`, which the message handler and
/// the coinbase worker share, as Core's `cs` serializes their calls.
pub type NodeHandler = ChainLockHandler<NodeClChain, NodeClVerify>;

/// The handler's chain: `ClIntake` and `RecordChainLock` on V, the published lock state, and
/// the node's `AbortNode`. Its calls wait for V, so never on V or a marked thread.
pub struct NodeClChain {
    v: ChainHandle,
    abort: Arc<dyn AbortSink>,
    #[cfg(test)]
    gate: ClGate,
}

impl NodeClChain {
    pub fn new(v: ChainHandle, abort: Arc<dyn AbortSink>) -> Self {
        NodeClChain {
            v,
            abort,
            #[cfg(test)]
            gate: ClGate::default(),
        }
    }

    /// The chain passing `gate` before each `ClIntake` and `RecordChainLock`.
    #[cfg(test)]
    pub(crate) fn gated(self, gate: &ClGate) -> Self {
        NodeClChain {
            gate: gate.clone(),
            ..self
        }
    }

    fn pass(&self, _point: ClPoint) {
        #[cfg(test)]
        self.gate.pass(_point);
    }
}

/// Where [`NodeClChain`] passes its test gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClPoint {
    Intake,
    Verify,
    Record,
}

/// A test's hook on [`NodeClChain`]'s and [`NodeClVerify`]'s commands, shared with the node's
/// `Chain`.
#[cfg(test)]
#[derive(Clone, Default)]
pub struct ClGate(pub Arc<Mutex<Option<ClHook>>>);

#[cfg(test)]
impl ClGate {
    pub(crate) fn pass(&self, point: ClPoint) {
        if let Some(gate) = &*self.0.lock().unwrap_or_else(PoisonError::into_inner) {
            gate(point);
        }
    }
}

/// The hook [`ClGate`] runs at each [`ClPoint`].
#[cfg(test)]
pub type ClHook = Box<dyn Fn(ClPoint) + Send>;

impl ClChain for NodeClChain {
    // Core: chainlock/handler.cpp:135, :145 at v24.0.0
    fn intake(&self) -> Result<i32, Stopped> {
        self.pass(ClPoint::Intake);
        self.v.cl_intake().recv().map(|r| r.best_height).map_err(|_| Stopped)
    }

    // Core: chainlock/handler.cpp:166-168; chainlock/chainlock.cpp:106-132 at v24.0.0
    fn record(
        &self,
        sig: Arc<ChainLockSig>,
        sig_hash: &[u8; 32],
        from: i64,
    ) -> Result<bool, Stopped> {
        self.pass(ClPoint::Record);
        let from = (from != -1).then_some(PeerId(from));
        let reply = self.v.record_chainlock(sig, Hash256::from_lendian(*sig_hash), from).recv();
        reply.map(|r| r.relay).map_err(|_| Stopped)
    }

    fn reads(&self) -> ClSigsSnapshot {
        let cl = self.v.view().cl.clone();
        ClSigsSnapshot {
            best: cl.best.clone(),
            best_hash: cl.best_hash.to_lendian(),
        }
    }

    fn abort_node(&self, msg: String) {
        self.abort.abort_node(&msg, None);
    }
}

/// The handler behind its lock.
#[derive(Clone)]
pub struct SharedHandler(Arc<Mutex<NodeHandler>>);

impl SharedHandler {
    pub fn new(handler: NodeHandler) -> Self {
        SharedHandler(Arc::new(Mutex::new(handler)))
    }

    pub fn lock(&self) -> MutexGuard<'_, NodeHandler> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl ChainLocks for SharedHandler {
    fn is_enabled(&self) -> bool {
        self.lock().is_enabled()
    }

    fn already_have(&self, hash: &[u8; 32]) -> bool {
        self.lock().already_have(hash)
    }

    fn process_new_chainlock(
        &mut self,
        from: i64,
        clsig: Read<ClSig>,
        hash: &[u8; 32],
    ) -> MessageProcessingResult {
        self.lock().process_new_chainlock(from, clsig, hash)
    }

    fn chainlock_by_hash(&self, hash: &[u8; 32], write: BlsCtx) -> Option<ClSig> {
        self.lock().chainlock_by_hash(hash, write)
    }

    fn best_chainlock(&self, write: BlsCtx) -> Option<ClSig> {
        self.lock().best_chainlock(write)
    }
}

/// The coinbase route's relay: Core's `PostProcessMessage(result, -1)`, which forgets the
/// requests for the inventory and relays it.
// Core: net_processing.cpp:2167-2169, :3819-3836 at v24.0.0
pub struct SharedRelay(pub Arc<Shared>);

impl ClRelay for SharedRelay {
    fn post(&self, result: MessageProcessingResult) {
        self.0.post_process_external(result);
    }
}
