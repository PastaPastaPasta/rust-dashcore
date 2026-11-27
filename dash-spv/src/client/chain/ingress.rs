//! The headers manager's way in to the header task (PLAN-r1 §2.2.3, SPV-LO9): a bounded queue
//! the manager never waits on, and a pause per peer while it is full. The task serves its
//! commands before this queue.

use std::collections::VecDeque;
use std::sync::Arc;

use dash_chain::PeerId;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, Notify};

use super::task::{stopped, HeadersReply};
use crate::bridge::headers::BridgedHeaders;
use crate::error::SpvError;

/// The batches the task's queue holds (PLAN-r1 §2.1).
pub const INGRESS_CAP: usize = 4;

/// One peer's `headers` message on its way to the task.
pub(super) struct Queued {
    pub(super) peer: PeerId,
    pub(super) headers: BridgedHeaders,
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
    /// The batch of each paused peer, in the order they were paused.
    kept: VecDeque<Queued>,
}

impl HeaderIngress {
    pub(super) fn new(tx: mpsc::Sender<Queued>, space: Arc<Notify>) -> Self {
        Self {
            tx,
            space,
            kept: VecDeque::new(),
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
        if self.paused(peer) {
            return Ok(Offered::Dropped);
        }
        let q = Queued {
            peer,
            headers,
        };
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

    /// Queues the kept batches, in the order their peers were paused, while the queue has room.
    /// Returns the peers resumed, each to be asked again with the chain's locator.
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
