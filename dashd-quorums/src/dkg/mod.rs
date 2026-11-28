//! The DKG runtime (DK1, LQ4b half; plan `PLAN-DK1.md`): Core's `CDKGSessionManager` and its
//! handlers over the pure session in `dash_llmq::dkg`.
//!
//! - [`types`]: the handler key, the phase and the `getdata` windows;
//! - [`seams`]: the traits the runtime reaches the node through;
//! - [`served`]: a session's recorded messages as other threads read them;
//! - [`handler`]: one handler's phase, quorum hash, height and session slot;
//! - [`wait`]: the handler's waits and `HandlePhase`;
//! - [`round`]: `InitNewQuorum` and `HandleDKGRound`;
//! - [`debug`]: the status `CDKGDebugManager` keeps;
//! - [`operator`]: the active masternode's operator key;
//! - [`runtime`]: the phase threads (`NetDKG::Start`, `Interrupt`, `Stop`);
//! - [`intake`]: ND1's network intake, pending queues and batch processor;
//! - [`log`]: a session's record lines and hashes as Core logs them.

pub mod debug;
pub mod handler;
pub mod intake;
pub mod log;
pub mod operator;
pub mod round;
pub mod runtime;
pub mod seams;
pub mod served;
pub mod types;
pub mod wait;

use std::collections::BTreeMap;
use std::sync::Arc;

use dash_llmq::dkg::session::DkgMsgKind;
use dash_llmq::dkg::wire::{Complaint, Contribution, Justification, PrematureCommitment};
use dash_params::{LlmqParams, LlmqType};

pub use debug::{DebugJsonSource, DebugManager, MemberStatus, SessionStatus};
pub use handler::Handler;
pub use operator::MnOperatorKey;
pub use runtime::{DkgFailStop, DkgPorts, DkgRuntime, RoundSeams, SimRates, StartError};
use seams::{ContributionsCache, DkgChain, Sporks};
pub use served::Served;
pub use types::{QuorumPhase, SessionKey, TipFacts, phase_serves};
pub use wait::{Abort, PhaseClock, PhaseCtx, TICK};

/// The seams [`DkgSessions`] reads on every tip and lookup.
#[derive(Clone)]
pub struct SessionSeams {
    /// `m_chainman`.
    pub chain: Arc<dyn DkgChain>,
    /// `m_sporkman`.
    pub sporks: Arc<dyn Sporks>,
    /// ND1's `contributionsCache`.
    pub cache: Arc<dyn ContributionsCache>,
}

/// `CDKGSessionManager`'s handlers and the lookups a `getdata` makes. On an active masternode
/// each handler is driven by its phase thread (L2); an observer's handlers stay Idle, without a
/// session, and ignore tips.
pub struct DkgSessions {
    handlers: Vec<Arc<Handler>>,
    active: bool,
    seams: SessionSeams,
}

impl DkgSessions {
    /// One handler per (type, quorum index): `signingActiveQuorumCount` for a rotated type, else
    /// one, in key order; a type given twice keeps its first parameters.
    // Core: v24.0.0 src/llmq/dkgsessionmgr.h:94-105 CDKGSessionManager::InitializeHandlers
    pub fn new(
        llmqs: impl IntoIterator<Item = (LlmqType, LlmqParams)>,
        seams: SessionSeams,
    ) -> Self {
        Self::with_handlers(llmqs, seams, true)
    }

    /// An observer's handlers: `CDKGSessionHandler`s whose phase is always Idle, whose lookups
    /// miss and whose tip update does nothing.
    // Core: v24.0.0 src/llmq/net_dkg.cpp:471-474 NetDKG::NetDKG
    // Core: v24.0.0 src/llmq/dkgsessionhandler.h:109-114 CDKGSessionHandler
    pub fn observer(
        llmqs: impl IntoIterator<Item = (LlmqType, LlmqParams)>,
        seams: SessionSeams,
    ) -> Self {
        Self::with_handlers(llmqs, seams, false)
    }

    fn with_handlers(
        llmqs: impl IntoIterator<Item = (LlmqType, LlmqParams)>,
        seams: SessionSeams,
        active: bool,
    ) -> Self {
        let mut handlers = BTreeMap::new();
        for (llmq_type, params) in llmqs {
            let count = if params.use_rotation {
                i32::from(params.signing_active_quorum_count)
            } else {
                1
            };
            for quorum_index in 0..count {
                let key = SessionKey {
                    llmq_type,
                    quorum_index,
                };
                handlers.entry(key).or_insert_with(|| Arc::new(Handler::new(key, params)));
            }
        }
        Self {
            handlers: handlers.into_values().collect(),
            active,
            seams,
        }
    }

    /// Whether the handlers are an active masternode's.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// The handlers' keys in order: `DoForHandler`'s membership.
    pub fn keys(&self) -> Vec<SessionKey> {
        self.handlers.iter().map(|h| h.key()).collect()
    }

    /// An active masternode's handlers in key order, for their phase threads; none for an
    /// observer.
    pub fn handlers(&self) -> &[Arc<Handler>] {
        if self.active {
            &self.handlers
        } else {
            &[]
        }
    }

    /// The phase of handler `key`: Idle for an observer or an unknown key.
    // Core: v24.0.0 src/llmq/dkgsessionhandler.h:113 CDKGSessionHandler::GetPhase
    pub fn phase(&self, key: SessionKey) -> QuorumPhase {
        self.handlers().iter().find(|h| h.key() == key).map_or(QuorumPhase::Idle, |h| h.phase())
    }

    /// Cleans ND1's contributions cache, then, outside the initial download, once DIP3 is
    /// enforced at the tip and while spork 17 is on, updates every handler.
    // Core: v24.0.0 src/llmq/dkgsessionmgr.cpp:41-55 CDKGSessionManager::UpdatedBlockTip
    pub fn updated_block_tip(&self, tip: &TipFacts, initial_download: bool) {
        self.seams.cache.cleanup_cache();
        if initial_download {
            return;
        }
        if !self.seams.chain.dip3_enforced(tip.height) {
            return;
        }
        if !self.seams.sporks.quorum_dkg_enabled() {
            return;
        }
        for handler in self.handlers() {
            handler.updated_block_tip(tip, self.seams.chain.as_ref());
        }
    }

    /// With spork 17 on, the first handler in key order whose phase serves `kind` and whose
    /// session answers `get`.
    fn lookup<T>(&self, kind: DkgMsgKind, get: impl Fn(&Handler) -> Option<T>) -> Option<T> {
        if !self.seams.sporks.quorum_dkg_enabled() {
            return None;
        }
        self.handlers().iter().filter(|h| phase_serves(kind, h.phase())).find_map(|h| get(h))
    }

    /// The contribution `hash`, for a `getdata`.
    // Core: v24.0.0 src/llmq/dkgsessionmgr.cpp:57-72 CDKGSessionManager::GetContribution
    pub fn get_contribution(&self, hash: &[u8; 32]) -> Option<Arc<Contribution>> {
        self.lookup(DkgMsgKind::Contribution, |h| h.get_contribution(hash))
    }

    /// The complaint `hash`, for a `getdata`.
    // Core: v24.0.0 src/llmq/dkgsessionmgr.cpp:74-89 CDKGSessionManager::GetComplaint
    pub fn get_complaint(&self, hash: &[u8; 32]) -> Option<Arc<Complaint>> {
        self.lookup(DkgMsgKind::Complaint, |h| h.get_complaint(hash))
    }

    /// The justification `hash`, for a `getdata`.
    // Core: v24.0.0 src/llmq/dkgsessionmgr.cpp:91-106 CDKGSessionManager::GetJustification
    pub fn get_justification(&self, hash: &[u8; 32]) -> Option<Arc<Justification>> {
        self.lookup(DkgMsgKind::Justification, |h| h.get_justification(hash))
    }

    /// The valid premature commitment `hash`, for a `getdata`.
    // Core: v24.0.0 src/llmq/dkgsessionmgr.cpp:108-123 CDKGSessionManager::GetPrematureCommitment
    pub fn get_premature_commitment(&self, hash: &[u8; 32]) -> Option<Arc<PrematureCommitment>> {
        self.lookup(DkgMsgKind::PrematureCommitment, |h| h.get_premature_commitment(hash))
    }
}

#[cfg(test)]
mod tests;
