//! `CDKGPendingMessages`: one handler's FIFO of one message kind, its seen-set and its
//! per-proTxHash quota.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Mutex, MutexGuard};

use dash_evo::types::ProTxHash;
use dash_llmq::dkg::session::Sender;
use dash_llmq::qdata::PeerId;
use tracing::debug;

use crate::dkg::log::uint256_hex;

/// A queued payload and who sent it.
pub type Pending = (Sender<PeerId>, Vec<u8>);

#[derive(Default)]
struct Inner {
    messages: VecDeque<Pending>,
    per_pro_tx: HashMap<ProTxHash, usize>,
    seen: HashSet<[u8; 32]>,
}

/// One queue. Its payloads wait for the handler's phase thread; the sessions stop before V at
/// shutdown (L3's order), and the queue reads no members.
// Core: v24.0.0 src/llmq/dkgsessionhandler.h:51-86 CDKGPendingMessages
pub struct PendingMessages {
    max_per_pro_tx: usize,
    inner: Mutex<Inner>,
}

impl PendingMessages {
    /// A queue that takes at most `max_per_pro_tx` messages from each proTxHash per round.
    pub fn new(max_per_pro_tx: usize) -> Self {
        Self {
            max_per_pro_tx,
            inner: Mutex::new(Inner::default()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `PushPendingMessage`: queues `payload` of hash `hash` from `from`, keyed for the quota by
    /// `sender` (the peer's verified proTxHash, or our own for [`Sender::Own`]). A hash seen
    /// before is dropped first; then a null `sender`; then a sender over its quota, whose
    /// message is not marked seen. The quota counts every message taken this round.
    // Core: v24.0.0 src/llmq/dkgsessionhandler.cpp:29-56 CDKGPendingMessages::PushPendingMessage
    pub fn push(&self, from: Sender<PeerId>, sender: &ProTxHash, payload: Vec<u8>, hash: [u8; 32]) {
        let mut inner = self.lock();
        if inner.seen.contains(&hash) {
            debug!(
                target: "llmq-dkg",
                "CDKGPendingMessages::PushPendingMessage -- already seen {}, peer={from}",
                uint256_hex(&hash)
            );
            return;
        }
        if sender.is_null() {
            return;
        }
        let count = inner.per_pro_tx.entry(*sender).or_insert(0);
        if *count >= self.max_per_pro_tx {
            debug!(
                target: "llmq-dkg",
                "CDKGPendingMessages::PushPendingMessage -- too many messages from {sender}, peer={from}"
            );
            return;
        }
        *count += 1;
        inner.seen.insert(hash);
        inner.messages.push_back((from, payload));
    }

    /// `PopPendingMessages(max)`: up to `max` payloads from the front.
    // Core: v24.0.0 src/llmq/dkgsessionhandler.cpp:58-69 CDKGPendingMessages::PopPendingMessages
    pub fn pop(&self, max: usize) -> Vec<Pending> {
        let mut inner = self.lock();
        let n = max.min(inner.messages.len());
        inner.messages.drain(..n).collect()
    }

    /// `HasSeen(hash)`.
    // Core: v24.0.0 src/llmq/dkgsessionhandler.cpp:71-75 CDKGPendingMessages::HasSeen
    pub fn has_seen(&self, hash: &[u8; 32]) -> bool {
        self.lock().seen.contains(hash)
    }

    /// `Clear()`: the payloads, the quota and the seen-set.
    // Core: v24.0.0 src/llmq/dkgsessionhandler.cpp:77-83 CDKGPendingMessages::Clear
    pub fn clear(&self) {
        *self.lock() = Inner::default();
    }
}
