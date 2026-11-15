//! The subscriber side (SB5 §3.4, §4.3): `next()` selects control and data in one slot section
//! (R1-M1, D24); the reader compresses in its own staging, outside the lock.

use std::collections::VecDeque;
use std::sync::Arc;

use dash_primitives::BlockHash;

use super::{ChainEvent, ControlMsg, FeedShared, FeedSlot, ReaderBuffers};
use crate::notify::Notify;
use crate::tip::TipStep;

/// What the subscriber does next. Control always comes before data (F4).
#[derive(Debug)]
pub enum Next<'a> {
    Control(ControlMsg),
    Event(&'a ChainEvent),
    Empty,
}

/// CH2's subscriber feed (CH2 §2.9.1, DEC-151 OQ1, with SB5's `next()`).
///
/// The driver step:
/// ```text
/// match feed.next() {
///     Next::Control(c) => handle_control(c),                  // re-reads the hub mirror
///     Next::Event(e)   => { /* decide on *e */ feed.pop(); }  // or compress, rule 3
///     Next::Empty      => { feed.set_waker(w); if let Next::Empty = feed.next() { park() } }
/// }
/// ```
pub trait SubscriberFeed: Send {
    /// ONE slot critical section: takes the oldest outstanding control message out of the slot
    /// (freeing its kind) and moves newly queued data events into staging. Returns that control
    /// message if there was one, otherwise peeks the head data event.
    fn next(&mut self) -> Next<'_>;

    /// Removes the head event. Valid only right after `next` returned `Event`, or after a
    /// `remove_round_trip` that found nothing since (debug assert).
    fn pop(&mut self) -> Option<ChainEvent>;

    /// Compression (CH2 §4.4.1 rule 3): if the head is `Connected(b)`, removes it and the first
    /// later `Disconnected(b)` and returns their `seq`s. Takes data only; control stays queued.
    /// A `None` leaves the head and `next`'s selection as they were, so the driver can `pop` the
    /// event it could not compress.
    fn remove_round_trip(&mut self, b: &BlockHash) -> Option<(u64, u64)>;

    /// F5. Park protocol: `set_waker`, then `next()` again; park only if it returns `Empty`.
    fn set_waker(&mut self, w: Notify);

    fn is_closed(&self) -> bool;
}

/// A live subscriber's reader. Dropping it closes the slot (D9).
pub struct FeedReader {
    slot: Arc<FeedSlot>,
    shared: Arc<FeedShared>,
    /// Events taken from the slot, oldest first; capacity ≥ Q.
    staging: VecDeque<ChainEvent>,
    /// The intake's swap target; capacity ≥ Q, empty between calls.
    spare: VecDeque<ChainEvent>,
    /// `next` returned `Event` and no call followed.
    selected: bool,
}

impl FeedReader {
    #[cfg_attr(not(test), allow(dead_code, reason = "F2's subscribe_at builds readers"))]
    pub(crate) fn new(
        slot: Arc<FeedSlot>,
        shared: Arc<FeedShared>,
        bufs: ReaderBuffers,
    ) -> FeedReader {
        FeedReader {
            slot,
            shared,
            staging: bufs.staging,
            spare: bufs.spare,
            selected: false,
        }
    }

    /// Events in staging.
    #[cfg(test)]
    pub(crate) fn staged(&self) -> usize {
        self.staging.len()
    }

    /// The capacities of staging and spare.
    #[cfg(test)]
    pub(crate) fn buffer_caps(&self) -> (usize, usize) {
        (self.staging.capacity(), self.spare.capacity())
    }

    /// A data-only intake: one O(1) section, then the append outside it.
    fn intake(&mut self) {
        let append = {
            let mut g = self.slot.lock();
            g.intake(&mut self.staging, &mut self.spare)
        };
        self.finish_intake(append);
    }

    fn finish_intake(&mut self, append: bool) {
        if append {
            // At most Q items into reserved capacity; `spare` keeps its buffer.
            self.staging.append(&mut self.spare);
        }
    }

    /// `n` events left staging: release their space and wake a waiting producer.
    fn released(&self, n: u32) {
        if self.slot.release_space(n) {
            self.shared.notify_producer();
        }
    }
}

impl SubscriberFeed for FeedReader {
    fn next(&mut self) -> Next<'_> {
        self.selected = false;
        let (ctl, append) = {
            let mut g = self.slot.lock();
            let ctl = g.take_control();
            let append = g.intake(&mut self.staging, &mut self.spare);
            self.slot.faults.in_intake(&mut self.staging);
            (ctl, append)
        };
        self.finish_intake(append);
        if let Some(c) = ctl {
            return Next::Control(c);
        }
        match self.staging.front() {
            Some(e) => {
                self.selected = true;
                Next::Event(e)
            }
            None => Next::Empty,
        }
    }

    fn pop(&mut self) -> Option<ChainEvent> {
        debug_assert!(self.selected, "pop without a selecting next()");
        self.selected = false;
        let e = self.staging.pop_front()?;
        self.released(1);
        Some(e)
    }

    fn remove_round_trip(&mut self, b: &BlockHash) -> Option<(u64, u64)> {
        self.intake();
        let connected = match self.staging.front()? {
            TipStep::Connected {
                block,
                seq,
            } if block.hash == *b => *seq,
            _ => return None,
        };
        let i =
            self.staging.iter().skip(1).position(
                |e| matches!(e, TipStep::Disconnected { block, .. } if block.hash == *b),
            )? + 1;
        let disconnected = self.staging.remove(i).expect("position is in range").seq();
        self.staging.pop_front();
        self.selected = false;
        self.released(2);
        Some((connected, disconnected))
    }

    fn set_waker(&mut self, w: Notify) {
        self.slot.set_waker(w);
    }

    fn is_closed(&self) -> bool {
        self.slot.is_closing()
    }
}

impl Drop for FeedReader {
    /// Closes the slot lock-free: `closing`, the producer wake, the close hook (D9).
    fn drop(&mut self) {
        self.slot.close(&self.shared);
    }
}
