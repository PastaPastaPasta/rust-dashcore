//! Step 7's request to V (CH2 §4.2; CH1-node §7.2): W asks V to clear the selection's blocks in
//! its index, and waits for the cleared records, which Bk or Bp writes, or for V's decline: V's
//! tip has fallen behind the selection (DEC-211), and W prunes nothing this flush.
//!
//! This is W's one wait on V. V answers it at every service point, holds included, because it
//! is index-only and never deferred (CH1-node §7.1, H3); W's wait is registered with the latch,
//! so a failure anywhere wakes it.

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use dash_chain::IndexWrite;

use super::prune::PruneRule;
use crate::store::{Fatal, GlobalFailStop, Parker};

/// W's `PruneClears`: V runs `prune_clear(files)` ([`FlushIndex::prune_clear`]) and
/// [`reply`](Self::reply)s with the cleared records, or [`decline`](Self::decline)s if `rule`
/// no longer allows `highest` at its tip ([`PruneRule::allows`]). Dropped unanswered, it stops
/// W's flush.
///
/// [`FlushIndex::prune_clear`]: super::FlushIndex::prune_clear
pub struct PruneClears {
    pub flush_id: u64,
    /// Step 7's selection, in file order.
    pub files: Vec<u32>,
    /// The highest block any of `files` holds.
    pub highest: u32,
    /// The rule `files` were chosen under.
    pub rule: PruneRule,
    reply: ClearsReply,
}

/// V's answer to a [`PruneClears`].
#[derive(Debug, PartialEq, Eq)]
pub enum ClearsAnswer {
    /// The records of the entries V cleared, in id order.
    Cleared(Vec<IndexWrite>),
    /// V's tip has fallen behind the selection (DEC-211): nothing is cleared.
    Declined,
}

/// The reply's transport. A plain latch-registered one-shot; if CH1-node needs `ReplySender`'s
/// semantics, this alias and [`ClearsWait`] are what change.
type ClearsReply = OneShot<ClearsAnswer>;

/// W's side of a [`PruneClears`].
pub(crate) type ClearsWait = OneShotWait<ClearsAnswer>;

impl PruneClears {
    /// A request for `files`, which hold blocks up to `highest`, chosen under `rule`; and W's
    /// wait for its reply.
    pub(crate) fn new(
        flush_id: u64,
        files: Vec<u32>,
        highest: u32,
        rule: PruneRule,
    ) -> (Self, ClearsWait) {
        let (reply, wait) = one_shot();
        let req = Self {
            flush_id,
            files,
            highest,
            rule,
            reply,
        };
        (req, wait)
    }

    /// V's answer: the records of the entries it cleared, in id order.
    pub fn reply(self, cleared: Vec<IndexWrite>) {
        self.reply.send(ClearsAnswer::Cleared(cleared));
    }

    /// V's answer when its tip has fallen behind the selection (DEC-211): W prunes nothing this
    /// flush, and arms the automatic check again.
    pub fn decline(self) {
        self.reply.send(ClearsAnswer::Declined);
    }
}

impl fmt::Debug for PruneClears {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PruneClears")
            .field("flush_id", &self.flush_id)
            .field("files", &self.files)
            .field("highest", &self.highest)
            .field("rule", &self.rule)
            .finish_non_exhaustive()
    }
}

/// Requests compare by what they ask.
impl PartialEq for PruneClears {
    fn eq(&self, other: &Self) -> bool {
        (self.flush_id, &self.files, self.highest, self.rule)
            == (other.flush_id, &other.files, other.highest, other.rule)
    }
}

impl Eq for PruneClears {}

enum State<T> {
    Pending,
    Sent(T),
    Dropped,
    Taken,
}

struct Slot<T> {
    state: Mutex<State<T>>,
    /// The waiter's wake: the reply, a drop, or (while it waits) the latch.
    parker: Arc<Parker>,
}

impl<T> Slot<T> {
    /// The state, also after a panic elsewhere: a drop while unwinding still wakes the waiter.
    fn state(&self) -> MutexGuard<'_, State<T>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The sending half: sent once, or dropped.
pub(crate) struct OneShot<T> {
    slot: Option<Arc<Slot<T>>>,
}

/// The waiting half.
pub(crate) struct OneShotWait<T> {
    slot: Arc<Slot<T>>,
}

/// What a [`OneShotWait`] found.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Polled<T> {
    Ready(T),
    /// The sender was dropped unanswered.
    Dropped,
    Pending,
}

fn one_shot<T>() -> (OneShot<T>, OneShotWait<T>) {
    let slot = Arc::new(Slot {
        state: Mutex::new(State::Pending),
        parker: Parker::new(),
    });
    (
        OneShot {
            slot: Some(Arc::clone(&slot)),
        },
        OneShotWait {
            slot,
        },
    )
}

impl<T> OneShot<T> {
    fn send(mut self, value: T) {
        self.close(State::Sent(value));
    }

    fn close(&mut self, state: State<T>) {
        if let Some(slot) = self.slot.take() {
            *slot.state() = state;
            slot.parker.notify();
        }
    }
}

impl<T> Drop for OneShot<T> {
    fn drop(&mut self) {
        self.close(State::Dropped);
    }
}

impl<T> OneShotWait<T> {
    /// Takes the reply once it has come; a drop is reported once too.
    pub(crate) fn poll(&self) -> Polled<T> {
        let mut state = self.slot.state();
        match std::mem::replace(&mut *state, State::Taken) {
            State::Sent(value) => Polled::Ready(value),
            State::Dropped => Polled::Dropped,
            State::Pending => {
                *state = State::Pending;
                Polled::Pending
            }
            State::Taken => panic!("a one-shot reply polled after it was taken"),
        }
    }

    /// Parks until the reply comes or the sender is dropped, registered with the latch before it
    /// parks (E-b1.7).
    ///
    /// # Errors
    ///
    /// [`Fatal`] once the latch has tripped with nothing come.
    pub(crate) fn park(&self, latch: &GlobalFailStop) -> Result<(), Fatal> {
        let _registered = latch.register(&self.slot.parker);
        loop {
            if !matches!(*self.slot.state(), State::Pending) {
                return Ok(());
            }
            latch.check()?;
            self.slot.parker.park();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use dash_consensus::primitives::BlockHash;
    use dashd_store::ErrorKind;

    use super::*;
    use crate::store::Failure;

    const AUTO: PruneRule = PruneRule::Automatic {
        after: 0,
    };

    fn record(n: u8) -> IndexWrite {
        IndexWrite {
            hash: BlockHash::from([n; 32]),
            bytes: vec![n],
        }
    }

    #[test]
    fn a_reply_is_taken_once_and_a_drop_is_reported() {
        let (req, wait) = PruneClears::new(4, vec![0, 2], 0, AUTO);
        assert_eq!((req.flush_id, req.files.as_slice()), (4, &[0, 2][..]));
        assert_eq!(wait.poll(), Polled::Pending);
        assert_eq!(wait.poll(), Polled::Pending, "a pending poll keeps the wait");
        req.reply(vec![record(1)]);
        assert_eq!(wait.poll(), Polled::Ready(ClearsAnswer::Cleared(vec![record(1)])));

        let (req, wait) = PruneClears::new(5, vec![1], 0, AUTO);
        drop(req);
        assert_eq!(wait.poll(), Polled::Dropped);

        let (req, wait) = PruneClears::new(6, vec![1], 0, PruneRule::Manual(9));
        req.decline();
        assert_eq!(wait.poll(), Polled::Ready(ClearsAnswer::Declined));
    }

    #[test]
    #[expect(clippy::disallowed_methods, reason = "plain test threads standing in for V")]
    fn the_wait_wakes_on_the_reply_the_drop_or_the_latch() {
        let latch = GlobalFailStop::new();
        // The reply from another thread, before or after W parks.
        let (req, wait) = PruneClears::new(1, vec![0], 0, AUTO);
        let v = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            req.reply(vec![record(7)]);
        });
        wait.park(&latch).unwrap();
        v.join().unwrap();
        assert_eq!(wait.poll(), Polled::Ready(ClearsAnswer::Cleared(vec![record(7)])));

        let (req, wait) = PruneClears::new(2, vec![0], 0, AUTO);
        let v = std::thread::spawn(move || drop(req));
        wait.park(&latch).unwrap();
        v.join().unwrap();
        assert_eq!(wait.poll(), Polled::Dropped);

        // The latch wakes a wait V never answers.
        let (req, wait) = PruneClears::new(3, vec![0], 0, AUTO);
        let tripper = latch.clone();
        let other = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            tripper.trip(Failure {
                kind: ErrorKind::Other,
                site: "test",
                message: "a failure".into(),
            });
        });
        assert!(wait.park(&latch).is_err());
        other.join().unwrap();
        assert_eq!(wait.poll(), Polled::Pending);
        drop(req);
    }
}
