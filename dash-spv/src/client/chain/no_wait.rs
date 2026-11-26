//! The no-wait marker (PLAN-r1 §2.1): a chain subscriber's pump awaits nothing but its own
//! feed, so the header writer, which waits on pumps, never waits on what a pump would wait on.
//! A pump runs marked; a header task reply or a task join awaited in a marked task fails at
//! once with [`SubError::WaitOnNoWaitTask`].
//!
//! The marker covers the header task's replies, task joins and store jobs only. It does not
//! cover [`IngressReplies`](super::IngressReplies), the ingress's `space().notified()`,
//! `source().changed()` or `SpvStore::run`: a pump awaiting one of those is not caught here.

use std::future::Future;
use std::pin::Pin;
use std::task::{ready, Context, Poll};

use dash_chain::ChainError;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::error::SpvError;

tokio::task_local! {
    static NO_WAIT: ();
}

/// A chain subscriber's failure.
#[derive(Debug, thiserror::Error)]
pub enum SubError {
    /// A chain read failed; the fail-stop has tripped.
    #[error("chain subscriber: {0}")]
    Storage(ChainError),
    /// A reply or a join was awaited in a task that may wait only on its feed.
    #[error("a chain subscriber pump waited on a reply or a join")]
    WaitOnNoWaitTask,
    /// The header task, or the subscribers, have stopped.
    #[error("the header chain has stopped")]
    Stopped,
}

impl From<SubError> for SpvError {
    fn from(e: SubError) -> SpvError {
        match e {
            SubError::Storage(e) => super::chain_error(e),
            SubError::WaitOnNoWaitTask => {
                SpvError::ChannelFailure("header task".into(), e.to_string())
            }
            SubError::Stopped => super::task::stopped(),
        }
    }
}

/// Starts `fut` as a marked task.
#[cfg_attr(not(test), allow(dead_code, reason = "the subscribers' pumps come next"))]
pub(crate) fn spawn_pump<F: Future<Output = ()> + Send + 'static>(fut: F) -> JoinHandle<()> {
    tokio::spawn(NO_WAIT.scope((), fut))
}

/// `Err` in a marked task.
pub(crate) fn check() -> Result<(), SubError> {
    match NO_WAIT.try_with(|_| ()) {
        Ok(()) => Err(SubError::WaitOnNoWaitTask),
        Err(_) => Ok(()),
    }
}

/// A header task reply; in a marked task it fails on its first poll.
pub struct SpvReply<T> {
    rx: oneshot::Receiver<T>,
    checked: bool,
}

impl<T> SpvReply<T> {
    pub(crate) fn new(rx: oneshot::Receiver<T>) -> Self {
        Self {
            rx,
            checked: false,
        }
    }
}

impl<T> Future for SpvReply<T> {
    type Output = Result<T, SubError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if !std::mem::replace(&mut self.checked, true) {
            check()?;
        }
        Poll::Ready(ready!(Pin::new(&mut self.rx).poll(cx)).map_err(|_| SubError::Stopped))
    }
}

/// A spawned task's join; in a marked task it fails on its first poll. A cancelled task is
/// [`SubError::Stopped`]; a panic resumes in the joiner.
pub struct SpvJoin<T> {
    join: JoinHandle<T>,
    checked: bool,
}

impl<T> SpvJoin<T> {
    pub(crate) fn new(join: JoinHandle<T>) -> Self {
        Self {
            join,
            checked: false,
        }
    }
}

impl<T> Future for SpvJoin<T> {
    type Output = Result<T, SubError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if !std::mem::replace(&mut self.checked, true) {
            check()?;
        }
        Poll::Ready(match ready!(Pin::new(&mut self.join).poll(cx)) {
            Ok(t) => Ok(t),
            Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
            Err(_) => Err(SubError::Stopped),
        })
    }
}
