//! Chain subscribers (PLAN-r1 §2.4): a no-wait pump per subscriber drains its feed and only
//! counts, and [`ChainSub::next`] builds every notice from two published snapshots, so a notice
//! never names a tip a step has not released.

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use dash_chain::{
    AncestryError, AnchoredAncestry, BlockRef, ChainError, ChainFeeds, FeedId, FeedSource, Handoff,
    Needs, Next, Notify, Refused, SubscribeError, SubscriberFeed,
};
use parking_lot::Mutex;
use tokio::sync::{watch, Notify as Signal};
use tokio::task::JoinHandle;

use super::no_wait::{spawn_pump, SubError};
use super::status::{tip_ref, HeaderSyncState};
#[cfg(test)]
use super::tests::sub::{At, LagLog, PollAt, Probe, PumpHook, Slot, TaskHooks};
use super::{ChainGen, ChainSource, LiveFeeds, PinnedChain, SourceState};
#[cfg(test)]
use dash_chain::ChainEvent;

/// The manager a subscriber serves, named in its log lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubKind {
    FilterHeaders,
    Filters,
    Blocks,
    Masternodes,
    Progress,
}

/// What [`ChainSub::next`] returns.
#[derive(Debug)]
pub enum SubItem {
    Notice(ChainNotice),
    /// The chain's caught-up, behind or established fact changed, at a tip no newer than the
    /// last notice's.
    Sync(HeaderSyncState),
    /// The header chain has stopped; nothing follows.
    Stop,
}

/// A change of the published chain. `pin` is the publication the notice was built from, of the
/// generation its ids belong to: `pin.snap.tip_seq == to_seq`, and its tip is the notice's new
/// tip. A handler reads the chain through `pin`, and keeps no id past a change of `pin.gen`.
#[derive(Clone, Debug)]
pub enum ChainNotice {
    Advanced {
        from: BlockRef,
        to: BlockRef,
        to_seq: u64,
        pin: PinnedChain,
    },
    /// The tip moved off `old_tip`'s branch; `fork` is the last common ancestor.
    Reorg {
        fork: BlockRef,
        old_tip: BlockRef,
        new_tip: BlockRef,
        disconnected: u32,
        connected: u32,
        to_seq: u64,
        pin: PinnedChain,
    },
    /// Start again from `pin`: the subscriber's own state may be on any branch, and every
    /// later notice follows from `pin`. `sync` is the state the task published at `pin`'s tip
    /// (`sync.tip_seq == pin.snap.tip_seq`), which a subscriber that fell behind may not have
    /// seen change.
    Resync {
        pin: PinnedChain,
        sync: HeaderSyncState,
        why: ResyncWhy,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResyncWhy {
    /// The subscriber's first notice.
    Start,
    /// The subscriber fell behind and its feed was closed.
    Lag,
    /// Its feed skipped a `seq`.
    Gap,
    /// The chain was replaced.
    Generation,
}

/// A pump's `popped` from its slot's handoff until the handoff's `seq` is known.
const UNSEEN: u64 = u64::MAX;

/// What a pump shares with its [`ChainSub`] and the writer's lag budget.
pub(super) struct Pump {
    /// The `seq` of the last event popped; [`UNSEEN`] until the handoff returns.
    popped: AtomicU64,
    /// Parked on an empty feed and not pushed to since: not what blocks the writer.
    parked: AtomicBool,
    closed: AtomicBool,
    gap: AtomicBool,
    wake: Signal,
    /// The times its feed's waker ran.
    #[cfg(test)]
    wakes: AtomicU64,
}

/// The live pumps, as the writer's lag budget reads them. `live()` runs under the tip lock: the
/// table's lock is a leaf below it (D52), held for no feed call.
pub(super) struct SubTable {
    subs: Mutex<Vec<SubEntry>>,
    next_key: AtomicU64,
    started: watch::Sender<bool>,
    #[cfg(test)]
    pub(super) lag: LagLog,
    #[cfg(test)]
    pub(super) task: TaskHooks,
}

#[derive(Clone)]
pub(super) struct SubEntry {
    key: u64,
    gen: ChainGen,
    pub(super) id: FeedId,
    pub(super) feeds: ChainFeeds,
    pump: Arc<Pump>,
    pub(super) kind: SubKind,
}

impl SubTable {
    pub(super) fn new() -> Arc<SubTable> {
        Arc::new(SubTable {
            subs: Mutex::new(Vec::new()),
            next_key: AtomicU64::new(0),
            started: watch::Sender::new(false),
            #[cfg(test)]
            lag: LagLog::default(),
            #[cfg(test)]
            task: TaskHooks::default(),
        })
    }

    /// The pumps of `gen`, copied: no feed call is made under the table's lock.
    pub(super) fn live(&self, gen: ChainGen) -> Vec<SubEntry> {
        self.subs.lock().iter().filter(|e| e.gen == gen).cloned().collect()
    }

    /// Closes the slot of every pump, of every generation, logging those it closes.
    pub(super) fn close_all(&self, why: &str) {
        let all = self.subs.lock().clone();
        for e in all.iter().filter(|e| e.close()) {
            tracing::info!("chain subscriber {:?}: feed closed ({why}), resync", e.kind);
        }
    }

    fn remove(&self, key: u64) {
        self.subs.lock().retain(|e| e.key != key);
    }

    /// Lets every subscriber register.
    pub(super) fn start(&self) {
        self.started.send_replace(true);
    }
}

impl SubEntry {
    /// The `seq` of the last event its pump popped; `None` while its handoff is returning.
    pub(super) fn popped(&self) -> Option<u64> {
        Some(self.pump.popped.load(Ordering::SeqCst)).filter(|&p| p != UNSEEN)
    }

    /// Its pump is parked on an empty feed that the writer has not pushed to since, so it is not
    /// what blocks the writer.
    pub(super) fn parked(&self) -> bool {
        self.pump.parked.load(Ordering::SeqCst)
    }

    /// Its slot was closed, by the writer or under its pump.
    pub(super) fn closed(&self) -> bool {
        self.pump.closed.load(Ordering::SeqCst)
    }

    /// Its slot is neither closed nor closing nor stale: a pump the lag budget counts. A
    /// subscriber retiring the slot deregisters it before it drops the entry.
    pub(super) fn open(&self) -> bool {
        !self.closed() && !self.feeds.is_closing(self.id)
    }

    #[cfg(test)]
    pub(super) fn wakes(&self) -> u64 {
        self.pump.wakes.load(Ordering::SeqCst)
    }

    /// Closes its slot and wakes its subscriber, which resyncs: a stuck pump never sees the
    /// close itself. Whether this call closed it: not when it was closed before, by the writer,
    /// a stop or its pump, nor when its slot is closing or stale, its subscriber retiring it.
    /// Logs nothing, as it may run under the tip lock.
    pub(super) fn close(&self) -> bool {
        if self.pump.closed.swap(true, Ordering::SeqCst) || self.feeds.is_closing(self.id) {
            return false;
        }
        self.feeds.deregister(self.id);
        self.pump.wake.notify_one();
        true
    }
}

/// One subscriber of the header chain, across its generations. Its first notice is a
/// `Resync { Start }`.
pub struct ChainSub {
    kind: SubKind,
    source: watch::Receiver<ChainSource>,
    status: watch::Receiver<HeaderSyncState>,
    started: watch::Receiver<bool>,
    /// The state of the last `Resync` or `Sync` returned.
    sync: Option<HeaderSyncState>,
    table: Arc<SubTable>,
    /// The generation of the last `Resync` returned.
    gen: Option<ChainGen>,
    /// Why the next registration resyncs, when it is the same generation's.
    why: ResyncWhy,
    live: Option<Live>,
    /// A slot a dropped `next()` left registered, with its lag-table key.
    stranded: Option<(ChainFeeds, FeedId, u64)>,
    #[cfg(test)]
    pub(super) hooks: Arc<super::tests::sub::Hooks>,
}

struct Live {
    feeds: ChainFeeds,
    id: FeedId,
    key: u64,
    pump: Arc<Pump>,
    task: JoinHandle<()>,
    /// The pin of the last notice returned.
    delivered: PinnedChain,
}

impl ChainSub {
    pub(super) fn new(
        kind: SubKind,
        source: watch::Receiver<ChainSource>,
        status: watch::Receiver<HeaderSyncState>,
        table: Arc<SubTable>,
    ) -> ChainSub {
        ChainSub {
            kind,
            source,
            status,
            started: table.started.subscribe(),
            sync: None,
            table,
            gen: None,
            why: ResyncWhy::Lag,
            live: None,
            stranded: None,
            #[cfg(test)]
            hooks: Arc::default(),
        }
    }

    pub fn kind(&self) -> SubKind {
        self.kind
    }

    /// The next notice or state change. Cancel-safe: a dropped call loses neither; the next
    /// call, or the drop, deregisters any slot it left. Nothing comes before the host starts
    /// its subscribers. While the chain is replaced it returns nothing; the new generation's
    /// first notice is a `Resync { Generation }`.
    pub async fn next(&mut self) -> Result<SubItem, SubError> {
        loop {
            if let Some((feeds, id, key)) = self.stranded.take() {
                self.release(&feeds, id, key);
            }
            let source = self.source.borrow_and_update().clone();
            let (feeds, reader) = match source.state {
                SourceState::Live {
                    feeds: LiveFeeds(feeds),
                    reader,
                } => (feeds, reader),
                SourceState::Replacing => {
                    self.detach();
                    #[cfg(test)]
                    self.hooks.at(At::Replacing).await;
                    self.source.changed().await.map_err(|_| SubError::Stopped)?;
                    continue;
                }
                SourceState::Stopped => {
                    self.detach();
                    return Ok(SubItem::Stop);
                }
            };
            if self.live.as_ref().is_some_and(|l| l.delivered.gen == source.gen) {
                if let Some(item) = self.deliver().await? {
                    return Ok(item);
                }
                continue;
            }
            self.detach();
            if !*self.started.borrow_and_update() {
                tokio::select! {
                    r = self.started.changed() => r.map_err(|_| SubError::Stopped)?,
                    r = self.source.changed() => r.map_err(|_| SubError::Stopped)?,
                }
                continue;
            }
            let why = match self.gen {
                None => ResyncWhy::Start,
                Some(g) if g != source.gen => ResyncWhy::Generation,
                Some(_) => self.why,
            };
            let pin = PinnedChain::new(&reader, source.gen, source.guard);
            if let Some(n) = self.attach(feeds, pin, why).await? {
                return Ok(SubItem::Notice(n));
            }
        }
    }

    /// The live feed's next notice, then a change of the chain's state at a tip already
    /// delivered; `None` once something changed that `next` must see.
    async fn deliver(&mut self) -> Result<Option<SubItem>, SubError> {
        let live = self.live.as_mut().expect("live");
        let pump = Arc::clone(&live.pump);
        let woke = pump.wake.notified();
        tokio::pin!(woke);
        woke.as_mut().enable();
        if pump.closed.load(Ordering::SeqCst) || pump.gap.load(Ordering::SeqCst) {
            self.why = match pump.gap.load(Ordering::SeqCst) {
                true => ResyncWhy::Gap,
                false => ResyncWhy::Lag,
            };
            tracing::debug!("chain subscriber {:?}: resync, {:?}", self.kind, self.why);
            self.detach();
            return Ok(None);
        }
        let now = live.delivered.repin();
        if now.snap.tip_seq > live.delivered.snap.tip_seq {
            let notice = match notice(&live.delivered, &now) {
                Err(AncestryError::Retired) => return Ok(None),
                r => r.map_err(unforked)?,
            };
            live.delivered = now;
            if let Some(n) = notice {
                return Ok(Some(SubItem::Notice(n)));
            }
        }
        let state = *self.status.borrow_and_update();
        let at = (live.delivered.gen, live.delivered.snap.tip_seq);
        if state.gen == at.0
            && state.tip_seq <= at.1
            && self.sync.is_none_or(|s| s.facts() != state.facts())
        {
            self.sync = Some(state);
            return Ok(Some(SubItem::Sync(state)));
        }
        #[cfg(test)]
        self.hooks.at(At::Parked).await;
        tokio::select! {
            _ = woke => {}
            r = self.source.changed() => r.map_err(|_| SubError::Stopped)?,
            _ = self.status.changed() => {}
        }
        Ok(None)
    }

    /// Registers on `pin`'s generation's feeds and hands off at its published tip (§2.4.2),
    /// then reads the state and re-reads the source (P7): only a source still `Live` at that
    /// generation, with the state of that generation at the pin's tip, gets the `Resync`. Any
    /// other read ends the attempt: the slot is released and `None` returned for `next` to hand
    /// off again from the top. A state behind the pin (its step's update still to come) is
    /// waited for, with the source, only after the release, so no undrained slot holds up the
    /// steps that update it. A state past the pin (a later step's) or a changed source ends it
    /// at once, and the next pin is no older than that state.
    ///
    /// Each retry is caused by a publication: a wait wakes only on a change of the state or the
    /// source, and an attempt ends at once only on a state its pin is older than, or on a
    /// source change, so the next attempt's pin is past what ended this one. An attempt whose
    /// pin and the state it reads match, at a source still `Live` at its generation, gets the
    /// `Resync`. There is no bound on the retries of one sub: publications that keep overtaking
    /// its attempts delay it for as long as they go on.
    async fn attach(
        &mut self,
        feeds: ChainFeeds,
        mut pin: PinnedChain,
        why: ResyncWhy,
    ) -> Result<Option<ChainNotice>, SubError> {
        let gen = pin.gen;
        #[cfg(test)]
        self.hooks.at(At::BeforeRegister).await;
        let Some(id) = self.after_hold(&feeds, |f| f.register(Needs::default())).await? else {
            return Ok(None);
        };
        #[cfg(test)]
        self.hooks.log.lock().push(Slot::Reg(id));
        let pump = Arc::new(Pump {
            popped: AtomicU64::new(UNSEEN),
            parked: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            gap: AtomicBool::new(false),
            wake: Signal::new(),
            #[cfg(test)]
            wakes: AtomicU64::new(0),
        });
        let key = self.table.next_key.fetch_add(1, Ordering::Relaxed);
        self.stranded = Some((feeds.clone(), id, key));
        let mut entry = Some(SubEntry {
            key,
            gen,
            id,
            feeds: feeds.clone(),
            pump: Arc::clone(&pump),
            kind: self.kind,
        });
        let table = Arc::clone(&self.table);
        #[cfg(test)]
        let hooks = Arc::clone(&self.hooks);
        let (feed, seq) = loop {
            let at = tip_ref(&pin.snap);
            let handoff = self
                .after_hold(&feeds, |f| {
                    let mut finish = || {
                        table.subs.lock().extend(entry.take());
                        #[cfg(test)]
                        hooks.finished(&table);
                        Ok(())
                    };
                    match FeedSource::<Infallible>::subscribe_at(f, id, &at, &mut finish) {
                        Err(SubscribeError::HoldActive) => Err(Refused::HoldActive),
                        r => Ok(r),
                    }
                })
                .await?;
            match handoff {
                None | Some(Err(SubscribeError::Stale | SubscribeError::Closing)) => {
                    return Ok(None)
                }
                Some(Ok(Handoff::Live {
                    feed,
                    seq_synced,
                })) => break (feed, seq_synced),
                Some(Ok(Handoff::Moved)) => pin = pin.repin(),
                Some(Err(SubscribeError::Latched)) => return Err(SubError::Stopped),
                Some(Err(SubscribeError::WouldDeadlock)) => {
                    return Err(SubError::FeedRefused(Refused::WouldDeadlock))
                }
                Some(Err(SubscribeError::AlreadyLive | SubscribeError::HoldActive)) => {
                    return Err(SubError::Storage(ChainError::Internal(
                        "a fresh slot's handoff refused past the hold protocol",
                    )))
                }
                Some(Err(SubscribeError::Finish(never))) => match never {},
            }
        };
        pump.popped.store(seq - 1, Ordering::SeqCst);
        #[cfg(test)]
        self.hooks.at(At::BeforeRecheck).await;
        #[cfg(test)]
        self.hooks.checks.fetch_add(1, Ordering::SeqCst);
        let sync = *self.status.borrow_and_update();
        let live = {
            let now = self.source.borrow_and_update();
            now.gen == gen && matches!(now.state, SourceState::Live { .. })
        };
        self.stranded = None;
        let (at, pinned) = ((sync.gen, sync.tip_seq), (gen, pin.snap.tip_seq));
        if !live || at != pinned {
            self.release(&feeds, id, key);
            if live && at < pinned {
                #[cfg(test)]
                self.hooks.at(At::Trailing).await;
                #[cfg(test)]
                let status = self.hooks.waiting(self.status.changed());
                #[cfg(not(test))]
                let status = self.status.changed();
                tokio::select! {
                    _ = status => {}
                    r = self.source.changed() => r.map_err(|_| SubError::Stopped)?,
                }
            }
            return Ok(None);
        }
        let task = spawn_pump(run_pump(
            feed,
            Arc::clone(&pump),
            #[cfg(test)]
            self.hooks.pump.lock().take(),
            #[cfg(test)]
            self.hooks.probe.lock().take(),
        ));
        self.live = Some(Live {
            feeds,
            id,
            key,
            pump,
            task,
            delivered: pin.clone(),
        });
        self.gen = Some(gen);
        self.sync = Some(sync);
        Ok(Some(ChainNotice::Resync {
            pin,
            sync,
            why,
        }))
    }

    /// `op` under SB5's hold protocol: on `HoldActive`, `on_release`, retry once, then wait for
    /// the release beside the source. `None` when the source changed first.
    async fn after_hold<T>(
        &mut self,
        feeds: &ChainFeeds,
        mut op: impl FnMut(&ChainFeeds) -> Result<T, Refused>,
    ) -> Result<Option<T>, SubError> {
        let mut armed: Option<Arc<Signal>> = None;
        loop {
            match op(feeds) {
                Ok(t) => return Ok(Some(t)),
                Err(Refused::HoldActive) => {}
                Err(Refused::Latched) => return Err(SubError::Stopped),
                Err(e) => return Err(SubError::FeedRefused(e)),
            }
            let Some(released) = armed.take() else {
                let s = Arc::new(Signal::new());
                feeds.on_release(Notify::Task(Waker::from(Arc::new(Wake(Arc::clone(&s))))));
                armed = Some(s);
                continue;
            };
            tokio::select! {
                _ = released.notified() => {}
                r = self.source.changed() => {
                    r.map_err(|_| SubError::Stopped)?;
                    return Ok(None);
                }
            }
        }
    }

    /// Ends the pump and closes its slot.
    fn detach(&mut self) {
        if let Some(l) = self.live.take() {
            l.task.abort();
            self.deregister(&l.feeds, l.id);
            self.table.remove(l.key);
        }
    }

    /// Closes a slot `next()` holds no pump for, and drops its lag-table entry if it has one.
    fn release(&self, feeds: &ChainFeeds, id: FeedId, key: u64) {
        self.deregister(feeds, id);
        self.table.remove(key);
    }

    fn deregister(&self, feeds: &ChainFeeds, id: FeedId) {
        #[cfg(test)]
        self.hooks.log.lock().push(Slot::Dereg(id));
        feeds.deregister(id);
    }
}

impl Drop for ChainSub {
    fn drop(&mut self) {
        self.detach();
        if let Some((feeds, id, key)) = self.stranded.take() {
            self.release(&feeds, id, key);
        }
    }
}

/// A [`Signal`] as a waker for dash-chain's wakes: notification only.
struct Wake(Arc<Signal>);

impl std::task::Wake for Wake {
    fn wake(self: Arc<Self>) {
        self.0.notify_one();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.notify_one();
    }
}

/// The notice from `from`'s tip to `to`'s, read through `to`: `None` when the tip is the same
/// block.
fn notice(from: &PinnedChain, to: &PinnedChain) -> Result<Option<ChainNotice>, AncestryError> {
    let (old, new) = (tip_ref(&from.snap), tip_ref(&to.snap));
    if old.id == new.id {
        return Ok(None);
    }
    let unheld = AncestryError::BelowFloor {
        height: to.floor(),
    };
    let fork = to.lca(old.id, new.id)?.ok_or(unheld)?;
    let (to_seq, pin) = (to.snap.tip_seq, to.clone());
    if fork == old.id {
        return Ok(Some(ChainNotice::Advanced {
            from: old,
            to: new,
            to_seq,
            pin,
        }));
    }
    let e = to.entry(fork)?;
    Ok(Some(ChainNotice::Reorg {
        fork: BlockRef {
            id: e.id,
            hash: e.hash,
            height: e.height,
        },
        old_tip: old,
        new_tip: new,
        disconnected: old.height - e.height,
        connected: new.height - e.height,
        to_seq,
        pin,
    }))
}

/// A notice's failed read: a store's, which has tripped the fail-stop, or two published tips
/// with no fork above the floor.
fn unforked(e: AncestryError) -> SubError {
    match e {
        AncestryError::Storage(e) => SubError::Storage(ChainError::Storage(e)),
        _ => SubError::Storage(ChainError::Internal("two published tips with no fork")),
    }
}

/// Events a pump pops before it yields.
const PUMP_BATCH: u32 = 64;

/// The pump (§2.4.1): pops every event, counts it, checks its `seq`, and wakes its
/// subscriber. Its only wait is its feed's waker.
async fn run_pump(
    mut feed: Box<dyn SubscriberFeed>,
    pump: Arc<Pump>,
    #[cfg(test)] mut hook: Option<PumpHook>,
    #[cfg(test)] probe: Option<Arc<Probe>>,
) {
    #[cfg(test)]
    {
        feed = ProbedFeed::wrap(feed, probe.clone(), &pump);
    }
    #[cfg(test)]
    PumpHook::start(&mut hook).await;
    let mut last = pump.popped.load(Ordering::SeqCst);
    loop {
        let mut popped = 0;
        while popped < PUMP_BATCH {
            if feed.is_closed() {
                pump.closed.store(true, Ordering::SeqCst);
                pump.wake.notify_one();
                return;
            }
            let seq = match feed.next() {
                Next::Control(_) => continue,
                Next::Event(e) => e.seq(),
                Next::Empty => break,
            };
            feed.pop();
            if seq != last + 1 {
                pump.gap.store(true, Ordering::SeqCst);
                pump.wake.notify_one();
                return;
            }
            last = seq;
            pump.popped.store(seq, Ordering::SeqCst);
            popped += 1;
            #[cfg(test)]
            PumpHook::after_pop(&mut hook).await;
        }
        if popped > 0 {
            pump.wake.notify_one();
        }
        match popped == PUMP_BATCH {
            true => tokio::task::yield_now().await,
            false => {
                let wait = FeedWait {
                    feed: &mut *feed,
                    pump: &pump,
                    #[cfg(test)]
                    probe: probe.as_deref(),
                };
                #[cfg(test)]
                PumpHook::park(&mut hook, wait).await;
                #[cfg(not(test))]
                wait.await
            }
        }
    }
}

/// Ready once the feed has an event or is closed; parked until then. `parked` is set only on an
/// empty feed, before the waker, so a push after the check clears it: the writer runs the waker
/// as it pushes, before its next lag check.
struct FeedWait<'a> {
    feed: &'a mut dyn SubscriberFeed,
    pump: &'a Arc<Pump>,
    #[cfg(test)]
    probe: Option<&'a Probe>,
}

impl FeedWait<'_> {
    fn ready(&mut self) -> bool {
        !matches!(self.feed.next(), Next::Empty) || self.feed.is_closed()
    }

    #[cfg(test)]
    fn probe(&self, at: PollAt) {
        if let Some(p) = self.probe {
            p.at(at, &self.pump.parked);
        }
    }
}

impl Future for FeedWait<'_> {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if !self.ready() {
            #[cfg(test)]
            self.probe(PollAt::Empty);
            self.pump.parked.store(true, Ordering::SeqCst);
            let wake = ParkWake {
                pump: Arc::clone(self.pump),
                inner: cx.waker().clone(),
            };
            self.feed.set_waker(Notify::Task(Waker::from(Arc::new(wake))));
            if !self.ready() {
                return Poll::Pending;
            }
        }
        self.pump.parked.store(false, Ordering::SeqCst);
        Poll::Ready(())
    }
}

/// A pump's feed waker: the pump is no longer parked once its feed has something for it.
struct ParkWake {
    pump: Arc<Pump>,
    inner: Waker,
}

impl std::task::Wake for ParkWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.pump.parked.store(false, Ordering::SeqCst);
        #[cfg(test)]
        self.pump.wakes.fetch_add(1, Ordering::SeqCst);
        self.inner.wake_by_ref();
    }
}

/// A pump's feed under a [`Probe`]: `set_waker` stops at the `WakerSet` gate once the waker is
/// installed, before it returns, and the next read at `Recheck`; that read, empty with no waker
/// run across it, completes a poll, which the probe records with the pump's `popped` and waker
/// runs.
#[cfg(test)]
struct ProbedFeed {
    inner: Box<dyn SubscriberFeed>,
    probe: Arc<Probe>,
    pump: Arc<Pump>,
    waker_set: bool,
}

#[cfg(test)]
impl ProbedFeed {
    fn wrap(
        feed: Box<dyn SubscriberFeed>,
        probe: Option<Arc<Probe>>,
        pump: &Arc<Pump>,
    ) -> Box<dyn SubscriberFeed> {
        match probe {
            Some(probe) => Box::new(ProbedFeed {
                inner: feed,
                probe,
                pump: Arc::clone(pump),
                waker_set: false,
            }),
            None => feed,
        }
    }
}

#[cfg(test)]
impl SubscriberFeed for ProbedFeed {
    fn next(&mut self) -> Next<'_> {
        let recheck = std::mem::take(&mut self.waker_set);
        if recheck {
            self.probe.at(PollAt::Recheck, &self.pump.parked);
        }
        let wakes = self.pump.wakes.load(Ordering::SeqCst);
        let next = self.inner.next();
        if recheck && matches!(next, Next::Empty) && self.pump.wakes.load(Ordering::SeqCst) == wakes
        {
            self.probe.polled(self.pump.popped.load(Ordering::SeqCst), wakes);
        }
        next
    }

    fn pop(&mut self) -> Option<ChainEvent> {
        self.inner.pop()
    }

    fn remove_round_trip(&mut self, b: &dash_primitives::BlockHash) -> Option<(u64, u64)> {
        self.inner.remove_round_trip(b)
    }

    fn set_waker(&mut self, w: Notify) {
        self.inner.set_waker(w);
        self.waker_set = true;
        self.probe.at(PollAt::WakerSet, &self.pump.parked);
    }

    fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }
}
