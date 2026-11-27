//! The DKG runtime's shared types (DK1 plan §3a).

use std::cmp::Ordering;
use std::hash::{Hash, Hasher};

pub use dash_llmq::dkg::phase::QuorumPhase;
use dash_llmq::dkg::session::DkgMsgKind;
pub use dash_llmq::qdata::TipRef;
use dash_params::LlmqType;
use dash_types::Numeric;

/// The block a tip notification names: `pindexNew`.
pub type TipFacts = TipRef;

/// A quorum's base block as the chain finds it: `pQuorumBaseBlockIndex`.
pub type BaseFacts = TipRef;

/// `CDKGSessionManager::SessionHandlerKey`: one handler per (type, quorum index), ordered as
/// Core's `std::map` orders them, by the type's number and then the index.
// Core: v24.0.0 src/llmq/dkgsessionmgr.h:44-48 SessionHandlerKey
#[derive(Clone, Copy, Debug)]
pub struct SessionKey {
    /// The quorum's type.
    pub llmq_type: LlmqType,
    /// The handler's quorum index.
    pub quorum_index: i32,
}

impl SessionKey {
    fn ordinal(&self) -> (u8, i32) {
        (self.llmq_type.to_base(), self.quorum_index)
    }
}

impl PartialEq for SessionKey {
    fn eq(&self, other: &Self) -> bool {
        self.ordinal() == other.ordinal()
    }
}

impl Eq for SessionKey {}

impl Hash for SessionKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.ordinal().hash(state);
    }
}

impl Ord for SessionKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.ordinal().cmp(&other.ordinal())
    }
}

impl PartialOrd for SessionKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Whether a handler in `phase` serves a `getdata` for a message of `kind`: contributions in
/// [Initialized, Contribute], complaints in [Contribute, Complain], justifications in
/// [Complain, Justify] and premature commitments in [Justify, Commit].
// Core: v24.0.0 src/llmq/dkgsessionmgr.cpp:64,81,98,115 CDKGSessionManager::Get*
pub fn phase_serves(kind: DkgMsgKind, phase: QuorumPhase) -> bool {
    let (first, last) = match kind {
        DkgMsgKind::Contribution => (QuorumPhase::Initialized, QuorumPhase::Contribute),
        DkgMsgKind::Complaint => (QuorumPhase::Contribute, QuorumPhase::Complain),
        DkgMsgKind::Justification => (QuorumPhase::Complain, QuorumPhase::Justify),
        DkgMsgKind::PrematureCommitment => (QuorumPhase::Justify, QuorumPhase::Commit),
        _ => return false,
    };
    !(phase < first || phase > last)
}
