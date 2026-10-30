//! Fixtures shared by the feed's unit tests and loom models.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use dash_consensus::BlockId;
use dash_primitives::BlockHash;

use super::{
    ChainEvent, Fanout, FeedConfig, FeedHandle, FeedId, FeedReader, FeedShared, FeedSlot, Handoff,
    Needs, Publish, Push, SubscriberFeed, TipCtx, TipFeeds, TipLock,
};
use crate::error::FailStop;
use crate::sync::{Condvar, Mutex};
use crate::tip::{BlockRef, TipStep};

pub(crate) fn block(k: u32) -> BlockRef {
    let mut h = [0u8; 32];
    h[..4].copy_from_slice(&k.to_le_bytes());
    BlockRef {
        id: BlockId(k),
        hash: BlockHash::from(h),
        height: k,
    }
}

pub(super) fn conn(k: u32, seq: u64) -> ChainEvent {
    TipStep::Connected {
        block: block(k),
        seq,
    }
}

pub(super) fn disc(k: u32, seq: u64) -> ChainEvent {
    TipStep::Disconnected {
        block: block(k),
        seq,
    }
}

/// A latch that records whether it tripped. Its own, since `crate::testing` is off under loom.
#[derive(Default)]
struct Latch(AtomicBool);

impl FailStop for Latch {
    fn read_failed(&self, _what: &str) {
        self.0.store(true, Ordering::SeqCst);
    }

    fn is_tripped(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// An untripped latch.
pub(crate) fn latch() -> Arc<dyn FailStop> {
    Arc::new(Latch::default())
}

pub(crate) fn cfg(q: u32) -> FeedConfig {
    FeedConfig {
        capacity: q,
        ..FeedConfig::node()
    }
}

impl FeedConfig {
    pub(super) fn with_max_feeds(self, max_feeds: u32) -> FeedConfig {
        FeedConfig {
            max_feeds,
            ..self
        }
    }
}

/// A handoff's `finish()` that succeeds.
pub(super) fn ok_finish() -> impl FnMut() -> Result<(), ()> {
    || Ok(())
}

/// A slot and its reader on `shared`, as `register` + `subscribe_at` will build them (F2).
pub(super) fn feed_on(shared: &Arc<FeedShared>, q: u32) -> (Arc<FeedSlot>, FeedReader) {
    let (slot, bufs) = FeedSlot::new(q);
    let r = FeedReader::new(slot.clone(), shared.clone(), bufs);
    (slot, r)
}

/// Feed state for queue size `q`, with a latch.
pub(crate) fn shared(q: u32) -> Arc<FeedShared> {
    Arc::new(FeedShared::new(cfg(q), latch()))
}

/// One feed on its own shared state.
pub(super) fn feed(q: u32) -> (Arc<FeedSlot>, Arc<FeedShared>, FeedReader) {
    let shared = shared(q);
    let (slot, r) = feed_on(&shared, q);
    (slot, shared, r)
}

/// The producer's push with its wake after the section, as fanout does it.
pub(super) fn push(slot: &FeedSlot, e: ChainEvent) -> bool {
    match slot.try_push(e) {
        Push::Pushed(w) => {
            if let Some(w) = w {
                w.notify();
            }
            true
        }
        Push::Full | Push::Closed => false,
    }
}

/// A model tip lock (§10.2): a user-level mutex, a flag under a facade mutex and condvar, so
/// loom's own `try_lock` (which reports false deadlocks) is never used. It holds the feed table
/// and the tip facts the subscriber side reads.
pub(crate) struct ModelTip {
    st: Mutex<Option<ModelState>>,
    cv: Condvar,
}

pub(crate) struct ModelState {
    pub(crate) feeds: TipFeeds,
    pub(crate) ctx: TipCtx,
}

/// The writer's (or a test's) hold on the model tip lock.
pub(crate) struct ModelGuard<'a> {
    tip: &'a ModelTip,
    st: Option<ModelState>,
}

impl ModelTip {
    /// A tip at `block(0)`, `next_seq` 1.
    pub(crate) fn new(cfg: &FeedConfig) -> Arc<ModelTip> {
        Arc::new(ModelTip {
            st: Mutex::new(Some(ModelState {
                feeds: TipFeeds::new(cfg),
                ctx: TipCtx {
                    tip: block(0),
                    next_seq: 1,
                    pre_genesis: false,
                },
            })),
            cv: Condvar::new(),
        })
    }

    /// The blocking lock, as the writer takes it.
    pub(crate) fn lock(&self) -> ModelGuard<'_> {
        let mut g = self.st.lock();
        loop {
            if let Some(st) = g.take() {
                return ModelGuard {
                    tip: self,
                    st: Some(st),
                };
            }
            g = self.cv.wait(g);
        }
    }
}

impl TipLock for ModelTip {
    fn try_with(&self, f: &mut dyn FnMut(&mut TipFeeds, TipCtx)) -> bool {
        let Some(mut st) = self.st.lock().take() else {
            return false;
        };
        f(&mut st.feeds, st.ctx);
        *self.st.lock() = Some(st);
        self.cv.notify_all();
        true
    }
}

impl std::ops::Deref for ModelGuard<'_> {
    type Target = ModelState;
    fn deref(&self) -> &ModelState {
        self.st.as_ref().expect("held")
    }
}

impl std::ops::DerefMut for ModelGuard<'_> {
    fn deref_mut(&mut self) -> &mut ModelState {
        self.st.as_mut().expect("held")
    }
}

impl Drop for ModelGuard<'_> {
    fn drop(&mut self) {
        *self.tip.st.lock() = self.st.take();
        self.tip.cv.notify_all();
    }
}

impl ModelGuard<'_> {
    /// The writer's own no-pending check before a step (§3.3), panicking with its error. Only
    /// writer models call it: the table itself must work when a writer breaks the rule (F1).
    #[cfg(loom)]
    pub(crate) fn before_step(&self) {
        if let Err(e) = self.feeds.before_step() {
            panic!("{e:?}");
        }
    }

    /// One node step: the next `seq`, then fanout (SB4's step + `after_publish`).
    fn step(
        &mut self,
        shared: &FeedShared,
        make: impl FnOnce(u64) -> ChainEvent,
    ) -> (ChainEvent, Publish) {
        let seq = self.ctx.next_seq;
        self.ctx.next_seq += 1;
        let e = make(seq);
        (e, self.feeds.fanout(shared, Fanout::One(e)))
    }

    /// Connects `b` on the tip.
    pub(crate) fn connect(&mut self, shared: &FeedShared, b: BlockRef) -> (ChainEvent, Publish) {
        self.ctx.tip = b;
        self.step(shared, |seq| TipStep::Connected {
            block: b,
            seq,
        })
    }

    /// Disconnects the tip, making `new_tip` the tip.
    pub(crate) fn disconnect(
        &mut self,
        shared: &FeedShared,
        new_tip: BlockRef,
    ) -> (ChainEvent, Publish) {
        let old = std::mem::replace(&mut self.ctx.tip, new_tip);
        self.step(shared, |seq| TipStep::Disconnected {
            block: old,
            seq,
        })
    }

    pub(crate) fn retry(&mut self, shared: &FeedShared) -> Publish {
        self.feeds.retry_pending(shared)
    }
}

/// A model chain's feeds: the tip, the shared state and the subscriber-side handle.
#[derive(Clone)]
pub(crate) struct Model {
    pub(crate) tip: Arc<ModelTip>,
    pub(crate) shared: Arc<FeedShared>,
    pub(crate) handle: FeedHandle,
}

impl Model {
    pub(crate) fn new(cfg: FeedConfig) -> Model {
        Model::with_latch(cfg, latch())
    }

    pub(crate) fn with_latch(cfg: FeedConfig, fail: Arc<dyn FailStop>) -> Model {
        let tip = ModelTip::new(&cfg);
        let shared = Arc::new(FeedShared::new(cfg, fail));
        let handle = FeedHandle {
            tip: tip.clone(),
            shared: shared.clone(),
        };
        Model {
            tip,
            shared,
            handle,
        }
    }

    /// `register` + `subscribe_at` at the current tip.
    pub(crate) fn subscribe(&self) -> (FeedId, Box<dyn SubscriberFeed>, u64) {
        let id = self.handle.register(Needs::default()).expect("registers");
        let tip = self.tip.lock().ctx.tip;
        match self.handle.subscribe_at(id, &tip, &mut ok_finish()) {
            Ok(Handoff::Live {
                feed,
                seq_synced,
            }) => (id, feed, seq_synced),
            Ok(Handoff::Moved) => unreachable!("nobody moves the tip here"),
            Err(e) => panic!("subscribe_at: {e:?}"),
        }
    }
}
