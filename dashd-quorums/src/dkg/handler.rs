//! One `ActiveDKGSessionHandler`'s shared state (DK1 plan §2, R-d): its phase and quorum hash
//! behind a leaf lock, its tip height, its session slot and its stop flag.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use dash_llmq::dkg::phase::phase_for_tip;
use dash_llmq::dkg::wire::{Complaint, Contribution, Justification, PrematureCommitment};
use dash_params::LlmqParams;
use dash_primitives::QuorumHash;
use tracing::debug;

use super::seams::DkgChain;
use super::served::Served;
use super::types::{QuorumPhase, SessionKey, TipFacts};

/// `phase` and `quorumHash`, which `cs_phase_qhash` guards.
struct PhaseHash {
    phase: QuorumPhase,
    quorum_hash: QuorumHash,
}

/// A handler for one (type, quorum index). The tip listener writes its phase, hash and height;
/// the phase thread owns its session and installs the session's [`Served`] in the slot; any
/// thread reads them. Each lock is a leaf.
pub struct Handler {
    key: SessionKey,
    params: LlmqParams,
    /// `cs_phase_qhash`.
    phase: Mutex<PhaseHash>,
    /// `currentHeight`.
    current_height: AtomicI32,
    /// `cs_session` and `curSession`.
    session: Mutex<Option<Arc<Served>>>,
    /// `stopRequested`.
    stop: AtomicBool,
}

impl Handler {
    /// A handler in Idle, with a null quorum hash, height -1 and no session.
    pub fn new(key: SessionKey, params: LlmqParams) -> Self {
        Self {
            key,
            params,
            phase: Mutex::new(PhaseHash {
                phase: QuorumPhase::Idle,
                quorum_hash: QuorumHash::default(),
            }),
            current_height: AtomicI32::new(-1),
            session: Mutex::new(None),
            stop: AtomicBool::new(false),
        }
    }

    /// The handler's type and quorum index.
    pub fn key(&self) -> SessionKey {
        self.key
    }

    /// The type's parameters.
    pub fn params(&self) -> &LlmqParams {
        &self.params
    }

    fn lock_phase(&self) -> MutexGuard<'_, PhaseHash> {
        self.phase.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn lock_session(&self) -> MutexGuard<'_, Option<Arc<Served>>> {
        self.session.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Sets the height, the quorum hash and, at a phase boundary, the phase from `tip`, and logs
    /// them before the phase lock is released. The base block's hash is read before the lock is
    /// taken.
    // Core: v24.0.0 src/active/dkgsessionhandler.cpp:43-70 ActiveDKGSessionHandler::UpdatedBlockTip
    pub fn updated_block_tip(&self, tip: &TipFacts, chain: &dyn DkgChain) {
        if chain.snapshot_unvalidated() {
            return;
        }
        let qi = self.key.quorum_index;
        if qi > 0 && !chain.rotation_enabled(&self.params, tip) {
            return;
        }
        let Some(at) = phase_for_tip(&self.params, qi, tip.height, false, true, QuorumPhase::Idle)
        else {
            return;
        };
        let base_hash = QuorumHash::from(*chain.ancestor_hash(tip, at.base_height).as_bytes());
        let mut state = self.lock_phase();
        let old = state.phase;
        let new = phase_for_tip(&self.params, qi, tip.height, false, true, old)
            .map_or(old, |tick| tick.phase);
        self.current_height.store(tip.height, Ordering::SeqCst);
        state.quorum_hash = base_hash;
        state.phase = new;
        debug!(
            target: "llmq-dkg",
            "ActiveDKGSessionHandler::UpdatedBlockTip -- {} qi[{qi}] currentHeight={}, pQuorumBaseBlockIndex->nHeight={}, oldPhase={}, newPhase={}",
            self.key.llmq_type,
            tip.height,
            at.base_height,
            old as i32,
            new as i32
        );
    }

    /// `GetPhase()`.
    // Core: v24.0.0 src/active/dkgsessionhandler.cpp:286-289 ActiveDKGSessionHandler::GetPhase
    pub fn phase(&self) -> QuorumPhase {
        self.lock_phase().phase
    }

    /// `GetCurrentQuorumHash()`.
    // Core: v24.0.0 src/active/dkgsessionhandler.cpp:72 ActiveDKGSessionHandler::GetCurrentQuorumHash
    pub fn quorum_hash(&self) -> QuorumHash {
        self.lock_phase().quorum_hash
    }

    /// `GetPhaseAndQuorumHash()`.
    // Core: v24.0.0 src/active/dkgsessionhandler.cpp:74-78 ActiveDKGSessionHandler::GetPhaseAndQuorumHash
    pub fn phase_and_quorum_hash(&self) -> (QuorumPhase, QuorumHash) {
        let state = self.lock_phase();
        (state.phase, state.quorum_hash)
    }

    /// `currentHeight`: the last tip's height, -1 before any.
    pub fn current_height(&self) -> i32 {
        self.current_height.load(Ordering::SeqCst)
    }

    /// `curSession = next`: installs a session's served side.
    // Core: v24.0.0 src/active/dkgsessionhandler.cpp:105 ActiveDKGSessionHandler::InitNewQuorum
    pub fn set_session(&self, served: Arc<Served>) {
        *self.lock_session() = Some(served);
    }

    /// `previous_session.swap(curSession)`: nulls the slot and returns what it held.
    // Core: v24.0.0 src/active/dkgsessionhandler.cpp:94-97 ActiveDKGSessionHandler::InitNewQuorum
    pub fn take_session(&self) -> Option<Arc<Served>> {
        self.lock_session().take()
    }

    /// `GetCurSession()`.
    // Core: v24.0.0 src/active/dkgsessionhandler.cpp:281-284 ActiveDKGSessionHandler::GetCurSession
    pub fn session(&self) -> Option<Arc<Served>> {
        self.lock_session().clone()
    }

    /// `RequestStop()`.
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    /// `IsStopRequested()`.
    pub fn stop_requested(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    /// The current session's contribution `hash`; none without a session.
    // Core: v24.0.0 src/active/dkgsessionhandler.cpp:257-261 ActiveDKGSessionHandler::GetContribution
    pub fn get_contribution(&self, hash: &[u8; 32]) -> Option<Arc<Contribution>> {
        self.session()?.contribution(hash)
    }

    /// The current session's complaint `hash`; none without a session.
    // Core: v24.0.0 src/active/dkgsessionhandler.cpp:263-267 ActiveDKGSessionHandler::GetComplaint
    pub fn get_complaint(&self, hash: &[u8; 32]) -> Option<Arc<Complaint>> {
        self.session()?.complaint(hash)
    }

    /// The current session's justification `hash`; none without a session.
    // Core: v24.0.0 src/active/dkgsessionhandler.cpp:269-273 ActiveDKGSessionHandler::GetJustification
    pub fn get_justification(&self, hash: &[u8; 32]) -> Option<Arc<Justification>> {
        self.session()?.justification(hash)
    }

    /// The current session's valid premature commitment `hash`; none without a session.
    // Core: v24.0.0 src/active/dkgsessionhandler.cpp:275-279 ActiveDKGSessionHandler::GetPrematureCommitment
    pub fn get_premature_commitment(&self, hash: &[u8; 32]) -> Option<Arc<PrematureCommitment>> {
        self.session()?.premature_commitment(hash)
    }
}
