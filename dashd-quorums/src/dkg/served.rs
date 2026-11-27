//! A session as other threads see it (DK1 plan §2, R-d; owner ruling Q1): Core's `invCs` maps,
//! which a `getdata` reads while the phase thread runs a batch, and our member index.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};

use dash_evo::llmq::OperatorKeyView;
use dash_llmq::dkg::session::{DkgSession, ServedSink};
use dash_llmq::dkg::wire::{Complaint, Contribution, Justification, PrematureCommitment};

#[derive(Default)]
struct Index {
    contributions: BTreeMap<[u8; 32], Arc<Contribution>>,
    complaints: BTreeMap<[u8; 32], Arc<Complaint>>,
    justifications: BTreeMap<[u8; 32], Arc<Justification>>,
    premature_commitments: BTreeMap<[u8; 32], Arc<PrematureCommitment>>,
    valid_commitments: BTreeSet<[u8; 32]>,
}

/// The served side of one session, which the handler's slot holds. The phase thread fills it
/// through [`ServedSink`]; any thread reads it. Its lock is a leaf: nothing else is called
/// under it.
pub struct Served {
    my_index: Option<usize>,
    index: Mutex<Index>,
}

impl Served {
    /// The served side of a session in which we are member `my_index` (`None` for an
    /// observer), with nothing recorded yet.
    pub fn new(my_index: Option<usize>) -> Self {
        Self {
            my_index,
            index: Mutex::new(Index::default()),
        }
    }

    /// The served side of `session`, before it records anything: DK1 attaches it with
    /// [`DkgSession::with_served`] and installs it with [`super::Handler::set_session`].
    pub fn for_session<E: OperatorKeyView>(session: &DkgSession<E>) -> Self {
        Self::new(session.my_index())
    }

    /// `AreWeMember()`.
    pub fn are_we_member(&self) -> bool {
        self.my_index.is_some()
    }

    /// `GetMyMemberIndex()`.
    pub fn my_index(&self) -> Option<usize> {
        self.my_index
    }

    fn lock(&self) -> MutexGuard<'_, Index> {
        self.index.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// A recorded contribution.
    // Core: v24.0.0 src/llmq/dkgsession.cpp:660-667 CDKGSession::GetContribution
    pub fn contribution(&self, hash: &[u8; 32]) -> Option<Arc<Contribution>> {
        self.lock().contributions.get(hash).cloned()
    }

    /// A recorded complaint.
    // Core: v24.0.0 src/llmq/dkgsession.cpp:669-676 CDKGSession::GetComplaint
    pub fn complaint(&self, hash: &[u8; 32]) -> Option<Arc<Complaint>> {
        self.lock().complaints.get(hash).cloned()
    }

    /// A recorded justification.
    // Core: v24.0.0 src/llmq/dkgsession.cpp:678-685 CDKGSession::GetJustification
    pub fn justification(&self, hash: &[u8; 32]) -> Option<Arc<Justification>> {
        self.lock().justifications.get(hash).cloned()
    }

    /// A recorded premature commitment, once it is valid.
    // Core: v24.0.0 src/llmq/dkgsession.cpp:687-694 CDKGSession::GetPrematureCommitment
    pub fn premature_commitment(&self, hash: &[u8; 32]) -> Option<Arc<PrematureCommitment>> {
        let index = self.lock();
        if !index.valid_commitments.contains(hash) {
            return None;
        }
        index.premature_commitments.get(hash).cloned()
    }
}

impl ServedSink for Served {
    // Core: v24.0.0 src/llmq/dkgsession.cpp:737 CDKGSession::ReceiveMessagePreamble
    fn contribution(&self, hash: &[u8; 32], qc: &Arc<Contribution>) {
        self.lock().contributions.entry(*hash).or_insert_with(|| qc.clone());
    }

    // Core: v24.0.0 src/llmq/dkgsession.cpp:739 CDKGSession::ReceiveMessagePreamble
    fn complaint(&self, hash: &[u8; 32], qc: &Arc<Complaint>) {
        self.lock().complaints.entry(*hash).or_insert_with(|| qc.clone());
    }

    // Core: v24.0.0 src/llmq/dkgsession.cpp:741 CDKGSession::ReceiveMessagePreamble
    fn justification(&self, hash: &[u8; 32], qj: &Arc<Justification>) {
        self.lock().justifications.entry(*hash).or_insert_with(|| qj.clone());
    }

    // Core: v24.0.0 src/llmq/dkgsession.cpp:575-582 CDKGSession::ReceiveMessage
    fn premature_commitment(&self, hash: &[u8; 32], qc: &Arc<PrematureCommitment>) {
        self.lock().premature_commitments.entry(*hash).or_insert_with(|| qc.clone());
    }

    // Core: v24.0.0 src/llmq/dkgsession.cpp:624 CDKGSession::ReceiveMessage
    fn valid_commitment(&self, hash: &[u8; 32]) {
        self.lock().valid_commitments.insert(*hash);
    }
}
