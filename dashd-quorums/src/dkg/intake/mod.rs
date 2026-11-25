//! ND1's DKG intake (Core's `NetDKG` network side and `CDKGPendingMessages`): the pending
//! queues of every handler, `ProcessMessage`, `AlreadyHave` and `ProcessGetData`, and the
//! batch processor the phase threads call. The verdicts and scores reach the node through
//! [`IntakeHost`], L3's adapter.
//!
//! - [`queues`]: one `CDKGPendingMessages`;
//! - [`message`]: `ProcessMessage`, `AlreadyHave` and `ProcessGetData`;
//! - [`batch`]: `ProcessPendingMessageBatch` and `RelayInvToParticipants`.

pub mod batch;
pub mod message;
pub mod queues;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use dash_dml::Dmn;
use dash_evo::types::ProTxHash;
use dash_kv::KvEngine;
use dash_llmq::dkg::session::{DkgEnv, DkgMsgKind, DkgSession, Sender};
use dash_llmq::qdata::PeerId;
use dash_params::{LlmqParams, LlmqType};
use dash_types::Numeric;

pub use batch::process_pending_batch;
pub use message::{DkgNetMsg, Verdict};
pub use queues::{Pending, PendingMessages};

use super::seams::{BatchProcessor, ContributionsCache, IntakeHost, PendingQueues};
use super::{DkgSessions, SessionKey, SessionSeams};
use crate::dkgdb::DkgDb;
use message::IndexCache;

/// The queues of one handler, one per kind in [`DkgMsgKind`]'s order.
type HandlerQueues = [PendingMessages; 4];

/// The queue of `kind` in a [`HandlerQueues`].
fn slot(kind: DkgMsgKind) -> Option<usize> {
    match kind {
        DkgMsgKind::Contribution => Some(0),
        DkgMsgKind::Complaint => Some(1),
        DkgMsgKind::Justification => Some(2),
        DkgMsgKind::PrematureCommitment => Some(3),
        _ => None,
    }
}

/// `NetDKG`'s intake over `CDKGSessionManager`'s handlers ([`DkgSessions`]).
pub struct DkgIntake {
    sessions: Arc<DkgSessions>,
    seams: SessionSeams,
    host: Arc<dyn IntakeHost>,
    llmqs: BTreeMap<u8, LlmqParams>,
    queues: BTreeMap<SessionKey, HandlerQueues>,
    indexed: Mutex<IndexCache>,
}

impl DkgIntake {
    /// The intake of `sessions`, built from the chain's `llmqs` (the list `sessions` was built
    /// from; a type given twice keeps its first parameters): each handler's four queues take
    /// `size * 2` messages per proTxHash, and each type's quorum-index cache holds
    /// `keepOldConnections` entries.
    // Core: v24.0.0 src/llmq/dkgsessionhandler.cpp:14-25 CDKGSessionHandler::CDKGSessionHandler
    // Core: v24.0.0 src/llmq/net_dkg.cpp:562-569 NetDKG::ProcessMessage
    pub fn new(
        sessions: Arc<DkgSessions>,
        llmqs: impl IntoIterator<Item = (LlmqType, LlmqParams)>,
        seams: SessionSeams,
        host: Arc<dyn IntakeHost>,
    ) -> Self {
        let mut by_type = BTreeMap::new();
        for (t, params) in llmqs {
            by_type.entry(t.to_base()).or_insert(params);
        }
        let queues = sessions
            .keys()
            .into_iter()
            .filter_map(|key| {
                let quota = usize::from(by_type.get(&key.llmq_type.to_base())?.size) * 2;
                Some((key, std::array::from_fn(|_| PendingMessages::new(quota))))
            })
            .collect();
        let indexed = Mutex::new(IndexCache::new(&by_type));
        Self {
            sessions,
            seams,
            host,
            llmqs: by_type,
            queues,
            indexed,
        }
    }

    fn queue(&self, key: SessionKey, kind: DkgMsgKind) -> Option<&PendingMessages> {
        Some(&self.queues.get(&key)?[slot(kind)?])
    }
}

/// The sessions stop before V at shutdown (L3's order), and no queue reads the members.
impl PendingQueues for DkgIntake {
    // Core: v24.0.0 src/llmq/dkgsessionhandler.cpp:85-91 CDKGSessionHandler::ClearPendingMessages
    fn clear(&self, key: SessionKey) {
        for queue in self.queues.get(&key).into_iter().flatten() {
            queue.clear();
        }
    }

    fn pop_batch(&self, key: SessionKey, kind: DkgMsgKind, max: usize) -> Vec<Pending> {
        self.queue(key, kind).map_or_else(Vec::new, |q| q.pop(max))
    }

    /// Hashes `payload` and queues it from [`Sender::Own`] under our proTxHash.
    // Core: v24.0.0 src/llmq/net_dkg.cpp:369-378 EnqueueOwn
    fn enqueue_own(&self, key: SessionKey, kind: DkgMsgKind, own: &ProTxHash, payload: Vec<u8>) {
        if let Some(queue) = self.queue(key, kind) {
            let hash = message::payload_hash(&payload);
            queue.push(Sender::Own, own, payload, hash);
        }
    }
}

/// [`process_pending_batch`] with [`IntakeHost`]. The sessions stop before V at shutdown
/// (L3's order), and the batch reads no members beyond the session's own.
impl BatchProcessor for DkgIntake {
    fn process_batch(
        &self,
        session: &mut DkgSession<Arc<Dmn>>,
        env: &mut DkgEnv<'_>,
        kind: DkgMsgKind,
        batch: Vec<(Sender<PeerId>, Vec<u8>)>,
    ) {
        process_pending_batch(self.host.as_ref(), session, env, kind, &batch);
    }
}

impl<E: KvEngine, C: crate::traits::DkgChain> ContributionsCache for DkgDb<E, C>
where
    Self: Send + Sync,
{
    // Core: v24.0.0 src/llmq/dkgsessionmgr.cpp:43 CDKGSessionManager::UpdatedBlockTip
    fn cleanup_cache(&self) {
        DkgDb::cleanup_cache(self);
    }
}
