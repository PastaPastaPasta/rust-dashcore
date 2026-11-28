//! The headers manager's way in to the header task (PLAN-r1 §2.2.3, SPV-LO9): a bounded queue
//! the manager never waits on, and a pause per peer while it is full. The task serves its
//! commands before this queue.

use std::collections::VecDeque;
use std::sync::{Arc, Weak};

use dash_chain::PeerId;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, watch, Notify};

use super::status::{HeaderSyncState, Round};
use super::task::{stopped, HeadersReply};
use crate::bridge::headers::BridgedHeaders;
use crate::error::SpvError;

/// The batches the task's queue holds (PLAN-r1 §2.1).
pub const INGRESS_CAP: usize = 4;

/// One peer's `headers` message, as the task's queue carries it, in order: with its round when
/// it is the sync peer's reply, applied at the batch's publication boundary, before the task
/// serves any command, if it asked the generation that takes the batch.
pub(super) struct Queued {
    pub(super) peer: PeerId,
    pub(super) headers: BridgedHeaders,
    pub(super) round: Option<Round>,
}

/// The task's answer to one queued batch, sent once its steps are committed and published.
#[derive(Debug)]
pub struct IngressReply {
    pub peer: PeerId,
    /// As [`HeaderHandle::headers`](super::HeaderHandle::headers) answers.
    pub reply: Result<HeadersReply, SpvError>,
}

/// What [`HeaderIngress::offer`] did with a batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Offered {
    /// In the task's queue; its reply comes on the replies channel.
    Queued,
    /// The queue was full: the batch is kept and the peer paused, so it is asked for no more
    /// headers until [`HeaderIngress::resume`] queues the batch.
    Paused,
    /// The peer is paused: the batch is dropped, without blame. After its resume the peer is
    /// asked again with the chain's locator.
    Dropped,
}

/// The sending side of the task's ingress, owned by the headers manager. No call waits on the
/// task: a full queue pauses the peer instead. A peer's batches reach the task in the order it
/// sent them, as a paused peer's later batches are dropped.
pub struct HeaderIngress {
    tx: mpsc::Sender<Queued>,
    space: Arc<Notify>,
    /// What a full queue held back, in order.
    kept: VecDeque<Queued>,
    /// The task's published state, which [`unknown_hash`](Self::unknown_hash) updates: held
    /// weakly, so the task stays its only lasting owner and the watch closes when it stops.
    status: Weak<watch::Sender<HeaderSyncState>>,
}

impl HeaderIngress {
    pub(super) fn new(
        tx: mpsc::Sender<Queued>,
        space: Arc<Notify>,
        status: Weak<watch::Sender<HeaderSyncState>>,
    ) -> Self {
        Self {
            tx,
            space,
            kept: VecDeque::new(),
            status,
        }
    }

    /// A peer announced a hash the chain does not hold, outside any round. Never queued and
    /// never dropped, paused or not: the observation is published before this returns, in one
    /// update of the state that bumps [`HeaderSyncState::observed`], records it
    /// [`outstanding`](HeaderSyncState::outstanding), and reports the chain behind and not
    /// caught up. From then until a [`Round`] asked at that `observed` or later is caught up,
    /// no publication reports caught up.
    ///
    /// The update takes the state watch's write lock: a caller must not hold a `borrow()` or
    /// `borrow_and_update()` guard of the state across this call, but copies the
    /// [`HeaderSyncState`] or [`Asked`](super::Asked) it needs and releases the borrow first.
    ///
    /// Once the task has stopped and dropped its sender, this does nothing and the watch is
    /// closed. The sender is held only for the update itself: a call that takes it just
    /// before the task drops its own publishes once, and the watch closes as the call returns.
    pub fn unknown_hash(&self) {
        if let Some(status) = self.status.upgrade() {
            status.send_modify(HeaderSyncState::observe);
        }
    }

    /// Queues `headers` from `peer` for the task, which takes each batch as one command. While
    /// any peer is paused the batch is kept and its peer paused too, so the kept batches go
    /// first.
    ///
    /// # Errors
    ///
    /// The task has stopped.
    pub fn offer(&mut self, peer: PeerId, headers: BridgedHeaders) -> Result<Offered, SpvError> {
        self.queue(Queued {
            peer,
            headers,
            round: None,
        })
    }

    /// [`offer`](Self::offer) for the sync peer's reply, `headers` (empty for an empty reply),
    /// with its `round`. The task applies the round once the batch has run, before it serves any
    /// command, and only when `round.asked` names the generation and tip that take the batch
    /// ([`Round`]). A dropped batch drops its round.
    ///
    /// # Errors
    ///
    /// The task has stopped.
    pub fn reply(
        &mut self,
        peer: PeerId,
        headers: BridgedHeaders,
        round: Round,
    ) -> Result<Offered, SpvError> {
        self.queue(Queued {
            peer,
            headers,
            round: Some(round),
        })
    }

    fn queue(&mut self, q: Queued) -> Result<Offered, SpvError> {
        if self.paused(q.peer) {
            return Ok(Offered::Dropped);
        }
        if !self.kept.is_empty() {
            self.kept.push_back(q);
            return Ok(Offered::Paused);
        }
        match self.tx.try_send(q) {
            Ok(()) => Ok(Offered::Queued),
            Err(TrySendError::Full(q)) => {
                self.kept.push_back(q);
                Ok(Offered::Paused)
            }
            Err(TrySendError::Closed(_)) => Err(stopped()),
        }
    }

    /// Whether `peer` is paused: it is sent no `getheaders`.
    pub fn paused(&self, peer: PeerId) -> bool {
        self.kept.iter().any(|q| q.peer == peer)
    }

    /// Queues the kept batches, in order, while the queue has room. Returns the peers resumed,
    /// each to be asked again with the chain's locator.
    ///
    /// # Errors
    ///
    /// The task has stopped.
    pub fn resume(&mut self) -> Result<Vec<PeerId>, SpvError> {
        let mut resumed = Vec::new();
        while let Some(q) = self.kept.pop_front() {
            let peer = q.peer;
            match self.tx.try_send(q) {
                Ok(()) => resumed.push(peer),
                Err(TrySendError::Full(q)) => {
                    self.kept.push_front(q);
                    break;
                }
                Err(TrySendError::Closed(_)) => return Err(stopped()),
            }
        }
        Ok(resumed)
    }

    /// Signalled each time the task takes a batch off the queue: the time to
    /// [`resume`](Self::resume). A stopped task signals nothing; its replies channel closes.
    pub fn space(&self) -> Arc<Notify> {
        Arc::clone(&self.space)
    }
}
