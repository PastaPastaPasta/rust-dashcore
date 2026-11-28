//! Core's signing manager (`LLMQContext::sigman`) behind `NetSigning`, and its "recsigs"
//! thread, as init builds, starts, interrupts and stops them.

use std::convert::Infallible;
use std::io;
use std::sync::Arc;

use dash_clock::Clock;
use dash_llmq::sigshares::recovery::LlmqRecovery;
use dash_llmq::sigshares::verify::WeightedVerifier;
use dash_p2p::PeerId;
use dash_p2p::rng::FastRandom;
use dash_p2p_core::Inventory;
use dash_params::ChainSetup;
use dash_primitives::TxHash;
use dashd_masternode::active::ActiveMasternode;
use dashd_masternode::sporks::SporkManager;
use dashd_p2p::Shared;
use dashd_quorums::signing::api::{ListenerId, RecoveredSigListener};
use dashd_quorums::signing::{
    QuorumSigning, RecSigNotify, RecSigsAbort, RecSigsWorker, RuntimeWork, SigningHandles,
    SigningInterrupt, SigningManager, SigningNet, SigningQuorums, SigningRuntime,
};
use dashd_quorums::sigshares::{SharesHandles, SigSharesRuntime, SigSharesThreads};
use dashd_quorums::{LocalMasternode, NotMasternode};
use rand_core::TryRng;

use super::quorums::NodeQuorums;
use super::recsigs::RecoveredSigsDb;
use crate::abort::AbortSink;
use crate::host::trace_thread;
use crate::shutdown::ShutdownState;
use crate::signing::shares::{
    ActiveIdentity, ActiveOperator, NodeShareFacts, ShareQueue, SharesNet, StrongRng,
};

/// The node's signing runtime: the recovered-signature database over route B.
pub type NodeSigningRuntime = SigningRuntime<RecoveredSigsDb, SigningQuorums>;

/// `ActiveContext::shareman` over the signing runtime and its route-B lookup.
pub type NodeSharesRuntime = SigSharesRuntime<
    SigningQuorums,
    NodeSigningRuntime,
    SharesNet,
    SharesNet,
    NodeShareFacts,
    LlmqRecovery,
>;

/// What an active masternode's `ActiveContext` adds to `NetSigning`: the share manager's
/// operator key, sporks, peer calls and scheme.
pub struct ActiveSigning {
    pub active: Arc<ActiveMasternode>,
    pub sporks: Arc<SporkManager>,
    pub net: Arc<SharesNet>,
    /// V's published scheme, Core's `bls::bls_legacy_scheme`.
    pub legacy: Box<dyn Fn() -> bool + Send + Sync>,
    /// The node's abort path, which must leave an earlier abort's cause in place.
    pub abort: Arc<dyn Fn(&str) + Send + Sync>,
}

/// The share manager's abort path: `AbortNode`, unless the node has already aborted, so the
/// first abort's cause stands.
pub fn share_abort(
    abort: Arc<dyn AbortSink>,
    shutdown: Arc<ShutdownState>,
) -> Arc<dyn Fn(&str) + Send + Sync> {
    Arc::new(move |what: &str| {
        if !shutdown.aborted() {
            abort.abort_node(what, None);
        }
    })
}

struct Shares {
    runtime: Arc<NodeSharesRuntime>,
    /// `ActiveContext`'s `cl_signer` and `is_signer`, registered before the share manager.
    signers: Vec<Arc<dyn RecoveredSigListener>>,
    listeners: Vec<ListenerId>,
    threads: Option<SigSharesThreads>,
}

/// `NetSigning`'s recovered-signature half: the runtime, and the "recsigs" thread from
/// [`start`](Self::start) until [`stop`](Self::stop); on an active masternode, also the share
/// manager and its threads.
pub struct NodeSigning {
    runtime: Arc<NodeSigningRuntime>,
    lookup: Arc<SigningQuorums>,
    interrupt: SigningInterrupt,
    worker: Option<RecSigsWorker>,
    shares: Option<Shares>,
}

impl NodeSigning {
    /// Core's `CSigningManager` over the node's recovered-signature database, its
    /// `-maxrecsigsage` and the manager's route-B lookup, with `NetSigning`'s peer manager and
    /// `NotifyRecoveredSig`'s subscribers (`PeerRelayRecoveredSig`; ZMQ's is not ported).
    /// With `active`, also `ActiveContext`'s share manager, as `AsyncSignIfMember`'s queue.
    // Core: v24.0.2 src/llmq/context.cpp:23, src/active/context.cpp:44, src/init.cpp:2165, :2202
    pub fn new(
        quorums: &NodeQuorums,
        setup: Arc<ChainSetup>,
        net: Arc<dyn SigningNet>,
        notify: Arc<dyn RecSigNotify>,
        clock: Arc<dyn Clock>,
        active: Option<ActiveSigning>,
    ) -> Self {
        let platform = setup.consensus.llmq_type_platform;
        let manager =
            SigningManager::new(quorums.recsigs.clone(), platform, quorums.max_recsigs_age);
        let lookup = Arc::new(quorums.signing_quorums(Arc::clone(&setup)));
        let masternode: Arc<dyn LocalMasternode + Send + Sync> = match &active {
            Some(a) => Arc::new(ActiveIdentity(Arc::clone(&a.active))),
            None => Arc::new(NotMasternode),
        };
        let handles = SigningHandles {
            net,
            notify,
            masternode,
            clock: Arc::clone(&clock),
        };
        let runtime = Arc::new(SigningRuntime::new(
            manager,
            Arc::clone(&lookup),
            Arc::clone(&setup),
            handles,
        ));
        let shares = active.map(|a| {
            let facts = NodeShareFacts {
                active: Arc::clone(&a.active),
                sporks: a.sporks,
                clock,
                legacy: a.legacy,
            };
            let shares = Arc::new(SigSharesRuntime::new(SharesHandles {
                lookup: Arc::clone(&lookup),
                intake: Arc::clone(&runtime),
                net: Arc::clone(&a.net),
                out: a.net,
                facts: Arc::new(facts),
                recovery: Arc::new(LlmqRecovery),
                operator: Arc::new(ActiveOperator(a.active)),
                chain: setup,
                abort: a.abort,
            }));
            let queue = Arc::new(ShareQueue(Arc::downgrade(&shares)));
            assert!(runtime.set_sign_queue(queue).is_ok(), "one share manager");
            Shares {
                runtime: shares,
                signers: Vec::new(),
                listeners: Vec::new(),
                threads: None,
            }
        });
        Self {
            runtime,
            lookup,
            interrupt: SigningInterrupt::new(),
            worker: None,
            shares,
        }
    }

    /// The runtime, where the composition registers listeners and sets the sign queue.
    pub fn runtime(&self) -> &Arc<NodeSigningRuntime> {
        &self.runtime
    }

    /// `ActiveContext::shareman`, on an active masternode.
    pub(crate) fn shares(&self) -> Option<&Arc<NodeSharesRuntime>> {
        self.shares.as_ref().map(|s| &s.runtime)
    }

    /// The runtime's route-B quorum lookup, `SelectQuorumForSigning`'s.
    pub(crate) fn lookup(&self) -> &Arc<SigningQuorums> {
        &self.lookup
    }

    /// Adds `ActiveContext`'s next signer (`cl_signer`, then `is_signer`), registered as a
    /// listener by [`start_active`](Self::start_active) before the share manager. Returns false,
    /// adding nothing, on a node that is not an active masternode.
    pub fn add_active_signer(&mut self, signer: Arc<dyn RecoveredSigListener>) -> bool {
        let Some(shares) = &mut self.shares else {
            return false;
        };
        assert!(shares.listeners.is_empty(), "signers are added before ActiveContext::Start");
        shares.signers.push(signer);
        true
    }

    /// `ActiveContext::Start`'s `RegisterRecoveryInterface` calls: the signers, then the share
    /// manager.
    // Core: v24.0.2 src/init.cpp:2370-2371, src/active/context.cpp:56-64
    pub fn start_active(&mut self) {
        let Some(shares) = &mut self.shares else {
            return;
        };
        assert!(shares.listeners.is_empty(), "ActiveContext started twice");
        let manager = Arc::clone(&shares.runtime) as Arc<dyn RecoveredSigListener>;
        for listener in shares.signers.iter().cloned().chain([manager]) {
            shares.listeners.push(self.runtime.register_listener(listener));
        }
    }

    /// `ActiveContext::Stop`'s `UnregisterRecoveryInterface` calls, in reverse.
    // Core: v24.0.2 src/init.cpp:335, src/active/context.cpp:66-74
    pub fn stop_active(&mut self) {
        if let Some(shares) = &mut self.shares {
            while let Some(id) = shares.listeners.pop() {
                self.runtime.unregister_listener(id);
            }
        }
    }

    /// `NetSigning::Start` from `StartHandlers`: "recsigs" under `trace_thread`, verifying under
    /// the scheme `legacy` gives (V's published one), drawing from `rng`, and reporting a fault
    /// to `abort`; then, on an active masternode, the share threads.
    // Core: v24.0.2 src/init.cpp:2363, src/llmq/net_signing.cpp:152-169
    pub fn start(
        &mut self,
        legacy: Box<dyn Fn() -> bool + Send + Sync>,
        rng: FastRandom,
        abort: Arc<dyn AbortSink>,
    ) -> io::Result<()> {
        assert!(self.worker.is_none(), "recsigs started twice");
        let work = RuntimeWork::new(Arc::clone(&self.runtime), legacy, SigningRng(rng));
        let abort: RecSigsAbort = Box::new(move |fault| abort.abort_node(&fault.to_string(), None));
        let worker =
            RecSigsWorker::start(Arc::new(work), self.interrupt.clone(), abort, |name, body| {
                trace_thread(name, body)
            })?;
        self.worker = Some(worker);
        // A share-thread start failure leaves "recsigs" running: init then fails, and the
        // shutdown that follows (`Node::shutdown`, also on drop) interrupts and stops it.
        if let Some(shares) = &mut self.shares {
            let verifier = || WeightedVerifier::new(StrongRng::default());
            shares.threads = Some(shares.runtime.start(self.interrupt.clone(), verifier)?);
        }
        Ok(())
    }

    // Core: v24.0.2 src/init.cpp:290-292, src/llmq/net_signing.h:61-63
    pub fn interrupt(&self) {
        self.interrupt.trigger();
    }

    /// Joins "recsigs", then the share threads; [`interrupt`](Self::interrupt) first.
    // Core: v24.0.2 src/init.cpp:337, src/llmq/net_signing.cpp:171-195
    pub fn stop(&mut self) {
        if let Some(mut worker) = self.worker.take() {
            worker.stop();
        }
        if let Some(threads) = self.shares.as_mut().and_then(|s| s.threads.take()) {
            threads.join();
        }
    }

    #[cfg(test)]
    pub(crate) fn is_started(&self) -> bool {
        self.worker.is_some()
    }

    #[cfg(test)]
    pub(crate) fn shares_started(&self) -> Option<bool> {
        self.shares.as_ref().map(|s| s.threads.is_some())
    }

    #[cfg(test)]
    pub(crate) fn active_listeners(&self) -> Option<usize> {
        self.shares.as_ref().map(|s| s.listeners.len())
    }
}

/// `NetSigning`'s `PeerMisbehaving`, `PeerRelayInv` and `PeerRelayTransaction`.
pub struct NodeSigningNet(pub Arc<Shared>);

impl SigningNet for NodeSigningNet {
    // Core: v24.0.2 src/net_processing.cpp:637
    fn misbehaving(&self, peer: PeerId, score: i32) {
        self.0.misbehaving(peer, score, "");
    }

    fn relay_inv(&self, inv: &Inventory) {
        self.0.relay_inv(inv);
    }

    fn relay_transaction(&self, txid: &TxHash) {
        self.0.relay_transaction(txid);
    }
}

/// `CSigningManager::rnd`, Core's `FastRandomContext`, as the pass's generator.
// Core: v24.0.2 src/llmq/signing.h:185
pub struct SigningRng(pub FastRandom);

impl TryRng for SigningRng {
    type Error = Infallible;

    fn try_next_u32(&mut self) -> Result<u32, Infallible> {
        Ok(self.0.rand32())
    }

    fn try_next_u64(&mut self) -> Result<u64, Infallible> {
        Ok(self.0.rand64())
    }

    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Infallible> {
        self.0.fill(dst);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
