//! A test producer that meets F1–F11 (CH2 §2.9.1, §7.2 b3's "test producer"): the seam's and
//! the CRASH suite's feed, where V is a script rather than SB5's `TipCell`.
//!
//! A tip mutex stands in for SB5's tip lock and a leaf mutex for its slots, so a reader's calls
//! never take the tip lock (DEC-157 (2)) and F10's `finish` may call into the table. The script's
//! tip changes assign
//! `seq` at once (F2) and emit to every live queue together once all have room (F3's atomic
//! fanout); an event assigned before a subscriber's cut is never delivered to it (F10). Controls
//! wait one per kind, a same-kind send replacing the payload (F4, DEC-155 (1)), and come before
//! data. Each enqueue and each control wakes the reader (F5); a pop retries the pending fanout, as
//! V does on its producer wake; a reader's drop closes its slot and
//! wakes W (F11, H4).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use dash_chain::{
    BlockRef, ChainEvent, ChainSnapshot, ControlMsg, Handoff, Needs, Next, Notify, Refused,
    SendControl, SubscribeError, SubscriberFeed, TipStep,
};
use dash_consensus::primitives::BlockHash;

use super::driver::event_seq;
use super::{FeedTable, SubscriberError, SubscriberId};

#[derive(Default)]
struct Slot {
    id: Option<SubscriberId>,
    /// F10's cut once live.
    seq_synced: Option<u64>,
    closing: bool,
    queue: VecDeque<ChainEvent>,
    controls: Vec<ControlMsg>,
    waker: Option<Notify>,
}

#[derive(Default)]
struct Inner {
    /// V's active chain, genesis first.
    chain: Vec<BlockRef>,
    next_seq: u64,
    capacity: usize,
    slots: Vec<Slot>,
    /// Assigned, not yet emitted (V's backpressure wait).
    pending: VecDeque<ChainEvent>,
    close_hook: Option<Notify>,
    /// The writer's wake on space (SB5's producer wake).
    producer: Option<Notify>,
    /// Run once inside `subscribe_at`, between `finish` and the cut.
    after_finish: Option<Box<dyn FnOnce() + Send>>,
}

impl Inner {
    fn slot(&mut self, id: SubscriberId) -> Option<&mut Slot> {
        self.slots.iter_mut().find(|s| s.id == Some(id))
    }

    fn targets(&self, seq: u64) -> impl Iterator<Item = usize> + '_ {
        self.slots
            .iter()
            .enumerate()
            .filter(move |(_, s)| !s.closing && s.seq_synced.is_some_and(|c| c <= seq))
            .map(|(i, _)| i)
    }

    /// Emits pending events while every target has room; the wakes to call after the section.
    fn emit(&mut self, wakes: &mut Vec<Notify>) {
        while let Some(&e) = self.pending.front() {
            let seq = event_seq(&e);
            let targets: Vec<usize> = self.targets(seq).collect();
            if targets.iter().any(|&i| self.slots[i].queue.len() >= self.capacity) {
                return;
            }
            self.pending.pop_front();
            for i in targets {
                self.slots[i].queue.push_back(e);
                wakes.extend(self.slots[i].waker.clone());
            }
        }
    }
}

fn event_block(e: &ChainEvent) -> BlockRef {
    match e {
        TipStep::Connected {
            block,
            ..
        }
        | TipStep::Disconnected {
            block,
            ..
        } => *block,
    }
}

/// The scripted producer: `Clone`, one table.
#[derive(Clone)]
pub struct ScriptedFeeds {
    /// Held across a tip change and across `subscribe_at`, `finish` included.
    tip: Arc<Mutex<()>>,
    inner: Arc<Mutex<Inner>>,
}

impl ScriptedFeeds {
    /// Q data events per subscriber, and V's active chain from genesis.
    pub fn new(capacity: usize, chain: Vec<BlockRef>) -> Self {
        Self {
            tip: Arc::default(),
            inner: Arc::new(Mutex::new(Inner {
                chain,
                next_seq: 1,
                capacity: capacity.max(1),
                ..Inner::default()
            })),
        }
    }

    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn tip_lock(&self) -> MutexGuard<'_, ()> {
        self.tip.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn with_wakes<T>(&self, f: impl FnOnce(&mut Inner, &mut Vec<Notify>) -> T) -> T {
        let mut wakes = Vec::new();
        let t = f(&mut self.inner(), &mut wakes);
        wakes.iter().for_each(Notify::notify);
        t
    }

    /// V's `ConnectTip(b)`: `seq` now, emission when every live queue has room.
    pub fn connect(&self, block: BlockRef) -> TipStep {
        let _tip = self.tip_lock();
        self.with_wakes(|i, wakes| {
            debug_assert!(i.chain.last().is_none_or(|t| t.height + 1 == block.height));
            let e = TipStep::Connected {
                block,
                seq: i.next_seq,
            };
            i.next_seq += 1;
            i.chain.push(block);
            i.pending.push_back(e);
            i.emit(wakes);
            e
        })
    }

    /// V's `DisconnectTip()`.
    ///
    /// # Panics
    ///
    /// With no tip.
    pub fn disconnect(&self) -> TipStep {
        let _tip = self.tip_lock();
        self.with_wakes(|i, wakes| {
            let block = i.chain.pop().expect("a tip");
            let e = TipStep::Disconnected {
                block,
                seq: i.next_seq,
            };
            i.next_seq += 1;
            i.pending.push_back(e);
            i.emit(wakes);
            e
        })
    }

    /// V's retry of its pending fanout; true once nothing is pending.
    pub fn retry_pending(&self) -> bool {
        self.with_wakes(|i, wakes| {
            i.emit(wakes);
            i.pending.is_empty()
        })
    }

    pub fn pending_is_empty(&self) -> bool {
        self.inner().pending.is_empty()
    }

    pub fn tip(&self) -> Option<BlockRef> {
        self.inner().chain.last().copied()
    }

    /// The `seq` of the last tip change (F2's `tip_seq`).
    pub fn tip_seq(&self) -> u64 {
        self.inner().next_seq - 1
    }

    /// V's hold of the tip lock, as a test holds it (H6).
    pub fn hold_tip(&self) -> MutexGuard<'_, ()> {
        self.tip_lock()
    }

    pub fn set_producer_wake(&self, w: Notify) {
        self.inner().producer = Some(w);
    }

    /// Runs `f` once, in the next `subscribe_at` that goes live, after its `finish` and before
    /// the subscriber is marked Synced (W's step 5 between the two; D37).
    pub fn set_after_finish(&self, f: impl FnOnce() + Send + 'static) {
        self.inner().after_finish = Some(Box::new(f));
    }

    /// Queued data events for `id`.
    pub fn queued(&self, id: SubscriberId) -> usize {
        self.inner().slot(id).map_or(0, |s| s.queue.len())
    }
}

impl FeedTable for ScriptedFeeds {
    fn register(&self, id: SubscriberId, _: Needs) -> Result<(), Refused> {
        self.inner().slots.push(Slot {
            id: Some(id),
            ..Slot::default()
        });
        Ok(())
    }

    fn deregister(&self, id: SubscriberId) {
        let hook = {
            let mut i = self.inner();
            i.slots.retain(|s| s.id != Some(id));
            i.close_hook.clone()
        };
        // F11: a pending event no longer waits on this queue.
        self.retry_pending();
        if let Some(h) = hook {
            h.notify();
        }
    }

    fn on_release(&self, w: Notify) {
        w.notify();
    }

    fn send_control(&self, id: SubscriberId, c: ControlMsg) -> SendControl {
        let (sent, wake) = {
            let mut i = self.inner();
            match i.slot(id) {
                Some(s) if !s.closing => {
                    let sent = match s.controls.iter().position(|m| m.kind() == c.kind()) {
                        Some(at) => {
                            s.controls.remove(at);
                            SendControl::Coalesced
                        }
                        None => SendControl::Queued,
                    };
                    s.controls.push(c);
                    (sent, s.waker.clone())
                }
                _ => (SendControl::Stale, None),
            }
        };
        if let Some(w) = wake {
            w.notify();
        }
        sent
    }

    fn is_closing(&self, id: SubscriberId) -> bool {
        self.inner().slot(id).is_none_or(|s| s.closing)
    }

    fn synced(&self) -> Vec<SubscriberId> {
        self.inner()
            .slots
            .iter()
            .filter(|s| s.seq_synced.is_some() && !s.closing)
            .filter_map(|s| s.id)
            .collect()
    }

    fn set_close_hook(&self, w: Notify) {
        self.inner().close_hook = Some(w);
    }

    fn snapshot(&self) -> Box<dyn ChainSnapshot + '_> {
        let i = self.inner();
        Box::new(Snapshot {
            chain: i.chain.clone(),
            tip_seq: i.next_seq - 1,
        })
    }

    fn subscribe_at(
        &self,
        id: SubscriberId,
        p: &BlockRef,
        finish: &mut dyn FnMut() -> Result<(), SubscriberError>,
    ) -> Result<Handoff, SubscribeError<SubscriberError>> {
        let _tip = self.tip_lock();
        let next_seq = {
            let mut i = self.inner();
            let next_seq = i.next_seq;
            let at_tip = i.chain.last() == Some(p);
            match i.slot(id) {
                None => return Err(SubscribeError::Stale),
                Some(s) if s.closing => return Err(SubscribeError::Closing),
                Some(s) if s.seq_synced.is_some() => return Err(SubscribeError::AlreadyLive),
                Some(_) if !at_tip => return Ok(Handoff::Moved),
                Some(_) => {}
            }
            next_seq
        };
        // The final commit under the tip lock (F10); the slots stay readable.
        finish().map_err(SubscribeError::Finish)?;
        let after = self.inner().after_finish.take();
        if let Some(f) = after {
            f();
        }
        match self.inner().slot(id) {
            Some(s) if !s.closing => s.seq_synced = Some(next_seq),
            _ => return Err(SubscribeError::Closing),
        }
        Ok(Handoff::Live {
            feed: Box::new(ScriptedReader {
                feeds: self.clone(),
                id,
                head: None,
            }),
            seq_synced: next_seq,
        })
    }
}

struct Snapshot {
    chain: Vec<BlockRef>,
    tip_seq: u64,
}

impl ChainSnapshot for Snapshot {
    fn tip(&self) -> BlockRef {
        *self.chain.last().expect("after genesis")
    }

    fn tip_seq(&self) -> u64 {
        self.tip_seq
    }

    fn at_height(&self, h: u32) -> Option<BlockRef> {
        self.chain.get(h as usize).copied()
    }
}

/// A live subscriber's reader. Dropping it closes the slot.
pub struct ScriptedReader {
    feeds: ScriptedFeeds,
    id: SubscriberId,
    /// The head `next` selected, as `Next::Event` lends it.
    head: Option<ChainEvent>,
}

impl SubscriberFeed for ScriptedReader {
    fn next(&mut self) -> Next<'_> {
        let mut i = self.feeds.inner();
        let Some(s) = i.slot(self.id) else {
            self.head = None;
            return Next::Empty;
        };
        if !s.controls.is_empty() {
            return Next::Control(s.controls.remove(0));
        }
        self.head = s.queue.front().copied();
        drop(i);
        match &self.head {
            Some(e) => Next::Event(e),
            None => Next::Empty,
        }
    }

    /// V retries its pending fanout at once on the space (its producer wake).
    fn pop(&mut self) -> Option<ChainEvent> {
        let e = self.feeds.with_wakes(|i, wakes| {
            let e = i.slot(self.id).and_then(|s| s.queue.pop_front());
            wakes.extend(i.producer.clone());
            i.emit(wakes);
            e
        });
        debug_assert_eq!(e, self.head.take(), "pop after next's Event");
        e
    }

    fn remove_round_trip(&mut self, b: &BlockHash) -> Option<(u64, u64)> {
        let got = self.feeds.with_wakes(|i, wakes| {
            let s = i.slot(self.id)?;
            let got = match s.queue.front() {
                Some(
                    e @ TipStep::Connected {
                        ..
                    },
                ) if event_block(e).hash == *b => {
                    let later = s.queue.iter().skip(1).position(|e| {
                        matches!(e, TipStep::Disconnected { .. }) && event_block(e).hash == *b
                    });
                    later.map(|at| {
                        let d = s.queue.remove(at + 1).expect("found");
                        let c = s.queue.pop_front().expect("the head");
                        (event_seq(&c), event_seq(&d))
                    })
                }
                _ => None,
            };
            if got.is_some() {
                wakes.extend(i.producer.clone());
                i.emit(wakes);
            }
            got
        });
        if got.is_some() {
            self.head = None;
        }
        got
    }

    fn set_waker(&mut self, w: Notify) {
        if let Some(s) = self.feeds.inner().slot(self.id) {
            s.waker = Some(w);
        }
    }

    fn is_closed(&self) -> bool {
        self.feeds.is_closing(self.id)
    }
}

impl Drop for ScriptedReader {
    fn drop(&mut self) {
        let hook = {
            let mut i = self.feeds.inner();
            if let Some(s) = i.slot(self.id) {
                s.closing = true;
                s.queue.clear();
            }
            i.close_hook.clone()
        };
        self.feeds.retry_pending();
        if let Some(h) = hook {
            h.notify();
        }
    }
}
