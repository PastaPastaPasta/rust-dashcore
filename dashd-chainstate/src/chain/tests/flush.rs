//! V's flush side through the model W (N7, §7.1, §7.2): Core's sites with the post-step pair,
//! and W's `PruneClears` answered at V's waits (H3, BL-123): inside a hold, inside SB4's
//! `enforce_block`, and idle; with DEC-211's check against the working tip.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, mpsc};
use std::time::Duration;

use dash_chain::{
    BlockRef, FeedSource, Handoff, IndexWrite, Needs, Next, Notify, Parker, Status, TipStep,
};
use dash_consensus::{Block, BlockId};
use dash_num::Hash256;
use dash_primitives::BlockHash;
use dash_types::Numeric as _;

use super::abc::adversarial;
use super::blocks::devnet;
use super::failure::RefusesDisconnect;
use super::support::{BeforeInit, Mined, Rig, RigConfig, branch};
use crate::chain::actor::COINS_ATTACH_MISUSE;
use crate::chain::reply::{ActorReply, ReplySender, WaitError, reply};
use crate::chain::{
    AbortCause, AbortSink as _, AttachCoins, BlockConnector, BlockStatusWriter, ChainCommand,
    ChainHandle, ChainLockSig, ConnectCx, ConnectOutcome, DisconnectResult, Fatal, FlushBell,
    FlushHost, FlushReleases, FlushSite, FlushStateMode, NullConnector, ProposalChecks, Queued,
    RecordingAbort, ShutdownFlag as _, ShutdownFlush, SiteDecision, VContext, ValidationInterface,
    ViewTarget, spawn_thread,
};
use crate::flush::{ClearsAnswer, FlushIndex as _, FlushSignals, PruneRule};

const SOON: Duration = Duration::from_secs(120);

/// An automatic prune's rule, with nPruneAfterHeight 0.
const AUTO: PruneRule = PruneRule::Automatic {
    after: 0,
};

/// W's step 7 as the model sends it: CH2b's `PruneClears` fields, with the model's own reply
/// (CH2b's one-shot is private to `flush`).
struct Request {
    files: Vec<u32>,
    highest: u32,
    rule: PruneRule,
    answer: ReplySender<Result<ClearsAnswer, String>>,
}

/// W's wait for V's answer to a [`Request`].
pub(super) type Answer = ActorReply<Result<ClearsAnswer, String>>;

/// One flush site of V's: its mode, and V's tip at the site and after the step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Site {
    mode: FlushStateMode,
    at: Option<(BlockRef, u64)>,
    after: Option<(BlockRef, u64)>,
}

#[derive(Default)]
struct Model {
    /// Each decision's flush is written and released at once: W hands V its `ChainStateFlushed`
    /// with the site's tip (DEC-166), through CH2b's port when `releases` is set.
    release_at_once: bool,
    releases: Option<FlushReleases>,
    bell: Option<FlushBell>,
    requests: VecDeque<Request>,
    /// Sent, with a ring, after the next site in this mode: AcceptBlock's NONE, or a tip step's
    /// IF_NEEDED, right after the step and before its drain.
    armed: Option<(FlushStateMode, Request)>,
    /// Each decision takes B1's dirty records, into `dirty`.
    take_dirty: bool,
    dirty: Vec<IndexWrite>,
    /// Calls to `service`, and V's tip height at each answer.
    services: usize,
    answered_at: Vec<Option<u32>>,
    /// A subscriber's `FlushDue` is pending (D47 item 10), and the open decision took it, as
    /// CH2b's does (`dmax_due`); V's tip height at each `answer_due`; and each `FlushDue`'s
    /// freeze, by a decision at its post-step pair or by `answer_due` at `(V's tip, the last
    /// seq)`.
    due: bool,
    took_due: bool,
    dues: Vec<Option<u32>>,
    due_freezes: Vec<(BlockRef, u64)>,
    /// A `FlushDue` raised, with a ring, after the next site in this mode, as `armed`.
    due_armed: Option<FlushStateMode>,
    /// Each prune-lock moveback (D61): the disconnected block's height, the number of sites
    /// before it, and of `DisconnectBlock`s committed by then (`commits`).
    movebacks: Vec<(u32, usize, usize)>,
    commits: Option<Arc<AtomicUsize>>,
    sites: Vec<Site>,
    open: Option<Site>,
    /// A job is in flight: V's `stop` waits for its release (D45).
    in_flight: bool,
    /// Told when V's `stop` begins; and the calls to it.
    stopping: Option<ReplySender<()>>,
    stops: usize,
    /// V's `stop` fails with this message, as a W whose last job failed does: it trips the
    /// latch and reports through W's sink, if it has one.
    stop_fails: Option<&'static str>,
    /// V's `stop` panics, as the adapter's `expect`s would.
    stop_panics: bool,
    /// The next command's flush fails with this message, unreported and the latch clear.
    flush_fails: Option<&'static str>,
    /// Jobs queued behind the one in flight, which W's end runs unless the latch has tripped.
    queued_jobs: usize,
    /// The store latch, which V's `abort` trips on a panic only (D62); each `abort`'s cause; and
    /// whether V's sink had reported by the trip.
    latched: bool,
    aborts: Vec<AbortCause>,
    /// W's job failed: it tripped the latch and reported this message, and every later call
    /// returns it as a `Fatal`.
    job_failed: Option<String>,
    sink: Option<Arc<RecordingAbort>>,
    latched_before_sink: Option<bool>,
}

/// The model W (§10.2 N7): a test thread stands in for W's step 7, sending through
/// [`ModelW::prune`] and waiting for V's answer. As V's [`FlushSite`] it decides nothing and
/// records each site.
#[derive(Clone, Default)]
pub(super) struct ModelW(Arc<(Mutex<Model>, Condvar)>);

/// CH2b's `AbortNode` for a `PruneClears` V dropped unanswered (`Halt::clears_dropped`).
const CLEARS_DROPPED: &str = "The block index dropped the prune clears unanswered";

/// The store guard's refusal of a write once the latch has tripped.
const FLUSH_REFUSED: &str = "refused: the store latch has tripped";

impl ModelW {
    pub(super) fn site(&self) -> Box<dyn FlushSite> {
        Box::new(self.clone())
    }

    /// A W whose own failure has tripped its latch and reported (L-3): V's `abort` finds it
    /// tripped, so V calls no sink.
    pub(super) fn tripped(self) -> Self {
        self.lock().latched = true;
        self
    }

    /// A W that writes and releases each decision's flush at once: V queues a
    /// `ChainStateFlushed` with the tip of each site that has one (DEC-166).
    pub(super) fn releasing() -> Self {
        let w = ModelW::default();
        w.lock().release_at_once = true;
        w
    }

    fn lock(&self) -> MutexGuard<'_, Model> {
        self.0.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Wakes a `stop` waiting for the release.
    fn notify(&self) {
        self.0.1.notify_all();
    }

    /// W's `PruneClears` for `files`, which hold blocks up to `highest`, under `rule`, and W's
    /// wait for the answer.
    fn request(files: &[u32], highest: u32, rule: PruneRule) -> (Request, Answer) {
        let (answer, wait) = reply();
        let r = Request {
            files: files.to_vec(),
            highest,
            rule,
            answer,
        };
        (r, wait)
    }

    /// W rings V's bell.
    fn ring(&self) {
        let bell = self.lock().bell.clone().expect("V attached its bell");
        bell.ring();
        self.notify();
    }

    /// W's job in flight is written and released, and W rings V's bell: every `DriverEvent`
    /// does, `Released` included (D60).
    fn release(&self) {
        let mut m = self.lock();
        m.in_flight = false;
        if let Some(bell) = &m.bell {
            bell.ring();
        }
        drop(m);
        self.notify();
    }

    /// W's job fails on W's thread, as CH2b's driver does (`flush/driver.rs:107`): it trips the
    /// store latch, reports through the sink, and rings V's bell; every later call returns the
    /// failure, which V must not report again (L-3).
    fn fail_job(&self, msg: &str) {
        {
            let mut m = self.lock();
            assert!(!m.latched, "one failure");
            m.latched = true;
            m.in_flight = false;
            m.job_failed = Some(msg.to_owned());
            m.sink.as_ref().expect("W's sink").abort_node(msg, None);
            if let Some(bell) = &m.bell {
                bell.ring();
            }
        }
        self.notify();
    }

    /// D62a: V stopped on one of Core's asserts, as on its panic: its flush side heard `Panic`,
    /// the latch tripped and the side was not stopped, so the shutdown flushes are refused.
    pub(super) fn assert_stopped_as_by_an_assert(&self) {
        self.assert_stopped_by_an_assert(&[AbortCause::Panic]);
    }

    /// CH3b D214: [`Self::assert_stopped_as_by_an_assert`] for an assert on an `AbortNode`'s
    /// `false`, where V had stopped already (D213): the flush side heard `AbortNode` there, then
    /// `run`'s `Panic`.
    pub(super) fn assert_asserted_after_an_abort_node(&self) {
        self.assert_stopped_by_an_assert(&[AbortCause::AbortNode, AbortCause::Panic]);
    }

    /// CH3b D213: V stopped on an `AbortNode` where it arose and again in `run`. Each `abort` was
    /// a latch read (D62), and the flush side was stopped once, at V's end, with the latch open,
    /// so the shutdown flushes run.
    pub(super) fn assert_stopped_on_an_abort_node(&self) {
        let m = self.lock();
        assert_eq!(m.aborts, [AbortCause::AbortNode, AbortCause::AbortNode]);
        assert_eq!((m.latched, m.stops), (false, 1));
        drop(m);
        assert_eq!(self.shutdown_flush(), Ok(()), "the store is open");
    }

    fn assert_stopped_by_an_assert(&self, aborts: &[AbortCause]) {
        let m = self.lock();
        assert_eq!(m.aborts, aborts);
        assert_eq!((m.latched, m.stops), (true, 0));
        drop(m);
        assert_eq!(self.shutdown_flush(), Err(FLUSH_REFUSED), "as Core's assert, no flush");
    }

    /// A store call of V's fails, as the guard handles it (`store/guarded.rs`): the latch trips
    /// and the failure is reported through the sink, before V sees its error.
    fn store_fails(&self, msg: &str) {
        let mut m = self.lock();
        assert!(!m.latched, "one failure");
        m.latched = true;
        m.sink.as_ref().expect("the guard's sink").abort_node(msg, None);
    }

    /// W's end once V has stopped (`ThreadDriver::drop`), and the number of queued jobs it
    /// runs: a request V left unanswered stops W's flush with CH2b's `AbortNode` unless the
    /// latch has tripped, and the queued jobs run unless it has.
    fn end(&self) -> usize {
        let mut m = self.lock();
        let dropped = std::mem::take(&mut m.requests);
        if !m.latched && !dropped.is_empty() {
            m.sink.as_ref().expect("W's sink").abort_node(CLEARS_DROPPED, None);
        }
        if m.latched {
            0
        } else {
            m.queued_jobs
        }
    }

    /// One of ND1's shutdown flushes on W's store once V has stopped (DEC-241): the guard refuses
    /// it, with this line, once the latch has tripped.
    fn shutdown_flush(&self) -> Result<(), &'static str> {
        if self.lock().latched {
            Err(FLUSH_REFUSED)
        } else {
            Ok(())
        }
    }

    /// The request sent and V's bell rung; W does not wait.
    pub(super) fn send(&self, files: &[u32], highest: u32) -> Answer {
        self.send_under(files, highest, AUTO)
    }

    /// [`send`](Self::send) under `rule`.
    fn send_under(&self, files: &[u32], highest: u32, rule: PruneRule) -> Answer {
        let (r, wait) = Self::request(files, highest, rule);
        self.lock().requests.push_back(r);
        self.ring();
        wait
    }

    /// The request sent, then W waits for V's answer.
    pub(super) fn prune(&self, files: &[u32], highest: u32) -> Result<ClearsAnswer, String> {
        self.send(files, highest).recv_timeout(SOON).expect("V answered in time").expect("V")
    }

    fn sites(&self) -> Vec<Site> {
        self.lock().sites.clone()
    }

    /// A subscriber's `FlushDue`, as CH2b's hub raises it, and W rings V's bell.
    fn flush_due(&self) {
        self.lock().due = true;
        self.ring();
    }

    /// The model once V's calls have made `done` true.
    fn wait_for(&self, done: impl Fn(&Model) -> bool) -> MutexGuard<'_, Model> {
        let (m, timeout) = self
            .0
            .1
            .wait_timeout_while(self.lock(), SOON, |m| !done(m))
            .unwrap_or_else(PoisonError::into_inner);
        assert!(!timeout.timed_out(), "V answered in time");
        m
    }

    /// The `FlushDue` freezes, once there are `n`.
    fn await_due_freezes(&self, n: usize) -> Vec<(BlockRef, u64)> {
        self.wait_for(|m| m.due_freezes.len() >= n).due_freezes.clone()
    }
}

impl FlushSite for ModelW {
    fn attach(&mut self, bell: FlushBell) {
        self.lock().bell = Some(bell);
    }

    fn at_site(
        &mut self,
        mode: FlushStateMode,
        host: &mut FlushHost<'_>,
    ) -> Result<SiteDecision, Fatal> {
        let at = host.tip();
        let mut m = self.lock();
        assert!(m.open.is_none(), "one decision at a time");
        m.took_due = std::mem::take(&mut m.due);
        m.open = Some(Site {
            mode,
            at,
            after: None,
        });
        Ok(SiteDecision::nothing())
    }

    fn after_step(&mut self, _: SiteDecision, host: &mut FlushHost<'_>) -> Result<(), Fatal> {
        let after = host.tip();
        let mut m = self.lock();
        let mut site = m.open.take().expect("a decision before its step");
        site.after = after;
        m.sites.push(site);
        if m.take_dirty {
            let dirty = host.take_dirty();
            m.dirty.extend(dirty);
        }
        // Genesis's connect has no tip before its step (CH1-A11).
        if let (true, Some((tip, _))) = (m.release_at_once, site.at) {
            match &m.releases {
                Some(port) => {
                    FlushSignals::chain_state_flushed(port, tip);
                    port.hand_to(host);
                }
                None => host.chain_state_flushed(tip),
            }
        }
        if m.armed.as_ref().is_some_and(|(mode, _)| *mode == site.mode)
            && let Some((_, r)) = m.armed.take()
        {
            m.requests.push_back(r);
            m.bell.as_ref().expect("V attached its bell").ring();
        }
        if std::mem::take(&mut m.took_due) {
            match after {
                // A cause kept for the job in flight (CH2b b3 D24), or the no-tip path (D47
                // item 6): the next decision or V's next wait answers it.
                Some(at) if !m.in_flight => m.due_freezes.push(at),
                _ => m.due = true,
            }
            self.notify();
        }
        if m.due_armed.take_if(|mode| *mode == site.mode).is_some() {
            m.due = true;
            m.bell.as_ref().expect("V attached its bell").ring();
        }
        Ok(())
    }

    fn service(&mut self, host: &mut FlushHost<'_>) -> Result<(), Fatal> {
        let mut m = self.lock();
        m.services += 1;
        if let Some(msg) = &m.job_failed {
            return Err(Fatal::new(msg.clone()));
        }
        answer(&mut m, host);
        Ok(())
    }

    /// CH2b's `FlushControl::answer_due` (D47 item 10): with no job in flight and a tip, a
    /// pending `FlushDue`, kept by a decision or not, is one freeze at `(V's tip, the last seq)`.
    fn answer_due(&mut self, host: &mut FlushHost<'_>) -> Result<(), Fatal> {
        self.service(host)?;
        let tip = host.tip();
        let mut m = self.lock();
        m.dues.push(tip.map(|(t, _)| t.height));
        if m.due
            && !m.in_flight
            && let Some(at) = tip
        {
            m.due = false;
            m.due_freezes.push(at);
        }
        self.notify();
        Ok(())
    }

    /// A command's flush, recorded as a site decided and run at once.
    fn flush(
        &mut self,
        mode: FlushStateMode,
        _: Option<u32>,
        host: &mut FlushHost<'_>,
    ) -> Result<(), Fatal> {
        let at = host.tip();
        let mut m = self.lock();
        if let Some(msg) = m.flush_fails.take() {
            return Err(Fatal::new(msg));
        }
        m.sites.push(Site {
            mode,
            at,
            after: at,
        });
        Ok(())
    }

    fn move_prune_locks_back(&mut self, height: u32) {
        let mut m = self.lock();
        assert!(m.open.is_none(), "the moveback comes before the step's decision");
        let n = m.sites.len();
        let c = m.commits.as_ref().map_or(0, |c| c.load(Ordering::SeqCst));
        m.movebacks.push((height, n, c));
    }

    /// CH2b's `wait_release` while a job is in flight, answering W's clears inline (D45).
    fn stop(&mut self, host: &mut FlushHost<'_>) -> Result<(), Fatal> {
        let mut m = self.lock();
        m.stops += 1;
        if let Some(stopping) = m.stopping.take() {
            stopping.send(());
        }
        assert!(!m.stop_panics, "W's stop panicked");
        if let Some(msg) = m.stop_fails {
            m.latched = true;
            if let Some(sink) = &m.sink {
                sink.abort_node(msg, None);
            }
            return Err(Fatal::new(msg));
        }
        loop {
            // A tripped latch ends the wait with a `Fatal`, before W's clears are answered, as
            // CH2b's `driver.wait` checks it.
            if m.latched {
                let msg = m.job_failed.as_deref().unwrap_or(FLUSH_REFUSED);
                return Err(Fatal::new(msg));
            }
            answer(&mut m, host);
            if !m.in_flight {
                return Ok(());
            }
            let _ = host.retry_pending();
            m = self.0.1.wait(m).unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// The adapter's: CH2b's `FlushControl::trip` on a panic; on an `AbortNode`, the latch read.
    fn abort(&mut self, cause: AbortCause, _: &mut FlushHost<'_>) -> bool {
        let mut m = self.lock();
        m.aborts.push(cause);
        if m.latched {
            return true;
        }
        if cause == AbortCause::Panic {
            m.latched = true;
            m.latched_before_sink = m.sink.as_ref().map(|s| s.messages().is_empty());
        }
        false
    }
}

/// CH2b's `FlushControl::handle` for the requests W sent: DEC-211 at the lower of V's working
/// and published tips (L-1), then the clears.
fn answer(m: &mut Model, host: &mut FlushHost<'_>) {
    while let Some(r) = m.requests.pop_front() {
        m.answered_at.push(host.tip().map(|(t, _)| t.height));
        if !host.prune_check_height().is_some_and(|h| r.rule.allows(r.highest, h)) {
            r.answer.send(Ok(ClearsAnswer::Declined));
            continue;
        }
        r.answer.send(host.prune_clear(&r.files).map(ClearsAnswer::Cleared));
    }
}

/// A stale two-block branch off genesis, accepted, with its block data moved to file 1, which
/// no step reads; and its ids.
fn stale_in_file_1(rig: &Rig) -> (Vec<Mined>, Vec<BlockId>) {
    let stale = branch(&rig.p, &rig.genesis, 2, 1);
    for m in &stale {
        rig.accept(m);
    }
    let ids = rig.ids(&stale);
    let moved = ids.clone();
    rig.on_v(move |v| {
        let mut ix = v.chain.node_index()?;
        for &id in &moved {
            ix.modify(id, |g| g.file = Some(1));
        }
        Ok(())
    });
    (stale, ids)
}

/// W's thread for a hold that a gated subscriber keeps waiting in its drain: once the subscriber
/// has stepped, W runs `act` (which waits for V's answer), checks that the published tip is
/// still `published` (V answered before its release), and lets the subscriber go.
fn w_in_hold<T: Send + 'static>(
    w: &ModelW,
    rig: &Rig,
    stepped: ActorReply<()>,
    go: ReplySender<()>,
    published: BlockId,
    act: impl FnOnce(&ModelW) -> T + Send + 'static,
) -> impl FnOnce() -> T {
    let (reader, w) = (rig.v.reader().clone(), w.clone());
    let t = spawn_thread("w", move || {
        stepped.recv().expect("S stepped");
        let answer = act(&w);
        assert_eq!(reader.snapshot().tip.tip, published, "answered inside the hold");
        go.send(());
        answer
    })
    .expect("spawn W");
    move || t.join().expect("unmarked joiner").expect("W")
}

/// `ids`' records as B1 would write them now.
fn records(rig: &Rig, ids: &[BlockId]) -> Vec<IndexWrite> {
    let ids = ids.to_vec();
    let (tx, rx) = reply();
    rig.on_v(move |v| {
        let ix = v.chain.node_index()?;
        tx.send(ids.iter().map(|&id| ix.record(id)).collect());
        Ok(())
    });
    rx.recv().expect("V")
}

/// A subscriber at Q = 1 that, once its first event has come, keeps its queue full until it
/// is told to go, so V's next tip step waits in V's in-hold drain. `stepped` says the first
/// event came; the subscriber then pops `n` events, and `done` joins it.
fn gated_subscriber(
    rig: &Rig,
    n: usize,
) -> (ActorReply<()>, ReplySender<()>, impl FnOnce() -> Vec<TipStep>) {
    let feeds = rig.v.query(|q| q.chain.feeds()).recv().expect("V");
    let id = feeds.register(Needs::default()).expect("registers");
    let tip = FeedSource::<()>::snapshot(&feeds).tip();
    let Ok(Handoff::Live {
        mut feed,
        ..
    }) = feeds.subscribe_at(id, &tip, &mut || Ok::<(), ()>(()))
    else {
        panic!("subscribe_at")
    };
    let (stepped_tx, stepped) = reply();
    let (go, go_rx) = reply::<()>();
    let s = spawn_thread("s", move || {
        let p = Parker::new();
        feed.set_waker(Notify::Thread(p.clone()));
        let mut gate = Some((stepped_tx, go_rx));
        let mut got = Vec::new();
        while got.len() < n {
            match feed.next() {
                Next::Event(_) => {}
                Next::Empty => {
                    assert!(p.park_timeout(SOON), "V never stepped");
                    continue;
                }
                Next::Control(_) => continue,
            }
            if let Some((stepped, go)) = gate.take() {
                stepped.send(());
                go.recv().expect("go");
            }
            got.push(feed.pop().expect("selected"));
        }
        got
    })
    .expect("spawn S");
    let done = move || {
        let got = s.join().expect("unmarked joiner").expect("S");
        feeds.deregister(id);
        got
    };
    (stepped, go, done)
}

fn block_ref(rig: &Rig, m: &Mined) -> BlockRef {
    BlockRef {
        id: rig.id(m),
        hash: m.hash(),
        height: m.height,
    }
}

/// §7.1 (D3): at each IF_NEEDED site V decides with the tip before the step, and runs the
/// decision at the post-step pair: `(parent(d), q+1)` after disconnecting `d`, `(id, q+1)` after
/// connecting `id`. ABC's PERIODIC and AcceptBlock's NONE run at `(V's tip, the last seq)`.
#[test]
fn the_flush_runs_at_cores_sites_with_the_post_step_pair() {
    let w = ModelW::default();
    let rig = Rig::with(RigConfig {
        flush: Some(w.site()),
        ..RigConfig::default()
    });
    let main = branch(&rig.p, &rig.genesis, 2, 0);
    let side = branch(&rig.p, &rig.genesis, 3, 1);
    for m in main.iter().chain(&side[..2]) {
        rig.accept(m);
    }
    let before = w.sites().len();
    let (_, q) = w.sites().last().and_then(|s| s.after).expect("a site");
    rig.accept(&side[2]);
    let sites = w.sites().split_off(before);
    let r = |m: &Mined| block_ref(&rig, m);
    let g = r(&rig.genesis);
    let pair = |m, at, k| Site {
        mode: FlushStateMode::IfNeeded,
        at: Some((at, q + k - 1)),
        after: Some((m, q + k)),
    };
    let want = [
        Site {
            mode: FlushStateMode::None,
            at: Some((r(&main[1]), q)),
            after: Some((r(&main[1]), q)),
        },
        pair(r(&main[0]), r(&main[1]), 1),
        pair(g, r(&main[0]), 2),
        pair(r(&side[0]), g, 3),
        pair(r(&side[1]), r(&side[0]), 4),
        pair(r(&side[2]), r(&side[1]), 5),
        Site {
            mode: FlushStateMode::Periodic,
            at: Some((r(&side[2]), q + 5)),
            after: Some((r(&side[2]), q + 5)),
        },
    ];
    assert_eq!(sites, want);
}

/// DEC-525: V's `Flush` command reaches the flush side in the mode it carries, each one call at
/// `(V's tip, the last seq)`, as Core's `FlushStateToDisk(state, mode)` from a caller with no tip
/// step.
// Core: src/validation.cpp:3024-3029 at the pin
#[test]
fn the_flush_command_passes_its_mode_at_vs_tip() {
    let w = ModelW::default();
    let rig = Rig::with(RigConfig {
        flush: Some(w.site()),
        ..RigConfig::default()
    });
    let main = branch(&rig.p, &rig.genesis, 1, 0);
    rig.accept(&main[0]);
    let before = w.sites().len();
    let at = w.sites().last().and_then(|s| s.after);
    assert_eq!(at.map(|(b, _)| b), Some(block_ref(&rig, &main[0])), "ABC's last site");
    let modes = [
        FlushStateMode::Always,
        FlushStateMode::IfNeeded,
        FlushStateMode::Periodic,
        FlushStateMode::None,
    ];
    for mode in modes {
        assert_eq!(rig.v.flush(mode).recv(), Ok(()), "{mode:?}");
    }
    let want = modes.map(|mode| Site {
        mode,
        at,
        after: at,
    });
    assert_eq!(w.sites().split_off(before), want);
}

/// BL-123 (H3): W asks for step 7's clears while V's hold waits in its drain for a subscriber;
/// V answers from that wait, before the release, so W's wait does not last the hold. The clears
/// are Core's (`PruneOneBlockFile`), and V's `-checkblockindex` after the reorg passes only with
/// `m_have_pruned` set.
#[test]
fn a_prune_flush_requested_during_a_hold_is_answered_inside_it() {
    let w = ModelW::default();
    let rig = Rig::with(RigConfig {
        feed_capacity: Some(1),
        flush: Some(w.site()),
        ..RigConfig::default()
    });
    // Past MIN_BLOCKS_TO_KEEP, so the stale branch's file is prunable at every tip below.
    let base = branch(&rig.p, &rig.genesis, 292, 0);
    for m in &base {
        rig.accept(m);
    }
    let (stale, stale_ids) = stale_in_file_1(&rig);
    // Three disconnects and four connects in one hold.
    let side = branch(&rig.p, &base[288], 4, 2);
    for m in &side[..3] {
        rig.accept(m);
    }
    let (stepped, go, done) = gated_subscriber(&rig, 7);
    let w_thread = w_in_hold(&w, &rig, stepped, go, rig.id(&base[291]), |w| w.prune(&[1], 2));
    rig.accept(&side[3]);
    let answer = w_thread();
    assert_eq!(done().len(), 7);
    assert_eq!(rig.v.view().snap.tip.tip, rig.id(&side[3]));
    assert_eq!(answer, Ok(ClearsAnswer::Cleared(records(&rig, &stale_ids))));
    for m in &stale {
        let e = rig.v.reader().entry(rig.id(m)).expect("held");
        assert!(!e.status.has(Status::HAVE_DATA) && e.file.is_none());
    }
    rig.assert_no_abort();
}

/// BL-123 inside SB4's `enforce_block` (§5.4): a ChainLock's marks disconnect the conflicting
/// chain while SB4 holds the tip; W's clears are answered from the drain of a mark's disconnect,
/// with the chain lent back to V, before SB4 stores its snapshot.
#[test]
fn a_prune_flush_while_enforce_block_holds_the_chain_is_answered() {
    let w = ModelW::default();
    let rig = Rig::with(RigConfig {
        chainlocks: true,
        feed_capacity: Some(1),
        flush: Some(w.site()),
        ..RigConfig::default()
    });
    let base = branch(&rig.p, &rig.genesis, 290, 0);
    let main = branch(&rig.p, &base[289], 3, 2);
    let side = branch(&rig.p, &base[289], 2, 3);
    for m in base.iter().chain(&main).chain(&side) {
        rig.accept(m);
    }
    let (_, stale_ids) = stale_in_file_1(&rig);
    rig.v.cl_intake().recv().expect("V replies");
    // a3's, a2's and a1's disconnects, then b1's and b2's connects; W is answered before SB4
    // stores its snapshot.
    let (stepped, go, done) = gated_subscriber(&rig, 5);
    let w_thread = w_in_hold(&w, &rig, stepped, go, rig.id(&main[2]), |w| w.prune(&[1], 2));
    let b2 = &side[1];
    let sig = Arc::new(ChainLockSig {
        height: b2.height as i32,
        block_hash: b2.hash(),
        sig: [7; 96],
    });
    let r = rig.v.record_chainlock(sig, Hash256::from_lendian([7; 32]), None).recv();
    assert!(r.expect("V replies").relay);
    let answer = w_thread();
    assert_eq!(done().len(), 5);
    assert_eq!(answer, Ok(ClearsAnswer::Cleared(records(&rig, &stale_ids))));
    assert_eq!(rig.v.view().snap.tip.tip, rig.id(b2));
    assert!(rig.status(&main[0]).has(Status::CONFLICT_CHAINLOCK));
    rig.assert_no_abort();
}

/// DEC-211: V checks a selection against its working tip, Core's `m_chain.Height()` inside the
/// hold, not the published one. Mid-reorg the working tip has fallen to h294, where a file
/// holding h7 is within MIN_BLOCKS_TO_KEEP (`294 − 288 = 6`) though the published h296 allows it;
/// V declines and clears nothing. Idle at the new tip h297 a selection to h9 clears.
#[test]
fn a_selection_the_working_tip_has_fallen_behind_is_declined() {
    let w = ModelW::default();
    let rig = Rig::with(RigConfig {
        feed_capacity: Some(1),
        flush: Some(w.site()),
        ..RigConfig::default()
    });
    let base = branch(&rig.p, &rig.genesis, 296, 0);
    for m in &base {
        rig.accept(m);
    }
    let (stale, stale_ids) = stale_in_file_1(&rig);
    // From h293: three disconnects (h296, h295, h294) and four connects in one hold.
    let side = branch(&rig.p, &base[292], 4, 2);
    for m in &side[..3] {
        rig.accept(m);
    }
    let (stepped, go, done) = gated_subscriber(&rig, 7);
    // V waits in the drain after h295's disconnect: its working tip is h294, while the
    // published one is still h296.
    let w_thread = w_in_hold(&w, &rig, stepped, go, rig.id(&base[295]), |w| w.prune(&[1], 7));
    rig.accept(&side[3]);
    let answer = w_thread();
    assert_eq!(done().len(), 7);
    assert_eq!(answer, Ok(ClearsAnswer::Declined));
    for m in &stale {
        assert!(rig.status(m).has(Status::HAVE_DATA), "declined: nothing cleared");
    }
    assert_eq!(rig.v.view().snap.tip.tip, rig.id(&side[3]));

    // V idle at h297: h9 is the last block it may prune.
    assert_eq!(w.prune(&[1], 10), Ok(ClearsAnswer::Declined));
    let answer = w.prune(&[1], 9);
    assert_eq!(answer, Ok(ClearsAnswer::Cleared(records(&rig, &stale_ids))));
    assert!(!rig.status(&stale[0]).has(Status::HAVE_DATA));
    rig.assert_no_abort();
}

/// L-1 (D46): inside a forward hold V's working tip is ahead of the published one, while a clear
/// is visible to view readers at once. DEC-211's check reads the lower of the two, so a
/// selection only the working tip allows is declined, and a later flush clears it. A subscriber
/// still holding h291's event makes h292's hold wait in its drain, where W's request, sent right
/// after the connect, is answered: published h291 leaves h3 prunable (MIN_BLOCKS_TO_KEEP), the
/// working h292 would allow h4. (`ActivateBestChainStep` releases once the tip's work beats the
/// old tip's, so a forward hold connects one block.)
#[test]
fn a_selection_only_a_forward_holds_working_tip_allows_is_declined() {
    let w = ModelW::default();
    let rig = Rig::with(RigConfig {
        feed_capacity: Some(1),
        flush: Some(w.site()),
        ..RigConfig::default()
    });
    let base = branch(&rig.p, &rig.genesis, 292, 0);
    for m in &base[..290] {
        rig.accept(m);
    }
    let (stale, stale_ids) = stale_in_file_1(&rig);
    let (_, go, done) = gated_subscriber(&rig, 2);
    rig.accept(&base[290]);
    let (r, answer) = ModelW::request(&[1], 4, AUTO);
    w.lock().armed = Some((FlushStateMode::IfNeeded, r));
    let connected = rig.v.process_block(base[291].block.clone(), true, None, true);
    assert_eq!(answer.recv_timeout(SOON), Some(Ok(Ok(ClearsAnswer::Declined))));
    assert_eq!(rig.v.reader().snapshot().tip.tip, rig.id(&base[290]), "answered in the hold");
    assert_eq!(w.lock().answered_at, [Some(292)]);
    go.send(());
    assert_eq!(done().len(), 2);
    assert!(connected.recv().expect("V replies").accepted);
    for m in &stale {
        assert!(rig.status(m).has(Status::HAVE_DATA), "declined: nothing cleared");
    }

    // Idle at h292, published too, the selection clears. An empty one clears nothing and leaves
    // `m_have_pruned` unset, as Core sets it only for a non-empty set (`v:3019-3023`).
    assert_eq!(w.prune(&[], 4), Ok(ClearsAnswer::Cleared(Vec::new())));
    assert!(!rig.v.query(|q| q.have_pruned).recv().expect("V serves"));
    let answer = w.prune(&[1], 4);
    assert_eq!(answer, Ok(ClearsAnswer::Cleared(records(&rig, &stale_ids))));
    assert!(!rig.status(&stale[0]).has(Status::HAVE_DATA));
    rig.assert_no_abort();
}

/// `ChainStateFlushed` with the locator of the site's tip, Core's `m_chain` before `SetTip`
/// (§7.1, OH-m1): queued at once outside a hold, staged inside one and spliced after its
/// publication (DEC-166, N1). §9.4's adversarial schedule runs the signals thread at the hold's
/// release, so a signal queued inside the hold would see a1 published.
fn chain_state_flushed_at(port: Option<FlushReleases>) {
    #[derive(Default)]
    struct Flushed {
        v: OnceLock<ChainHandle>,
        /// The locator's tip, and the published tip when the listener ran.
        log: Mutex<Vec<(BlockHash, BlockId)>>,
    }
    impl ValidationInterface for Flushed {
        fn chain_state_flushed(&self, locator: &[BlockHash]) {
            let published = self.v.get().expect("set").view().snap.tip.tip;
            self.log.lock().expect("log").push((locator[0], published));
        }
    }
    let w = ModelW::default();
    {
        let mut m = w.lock();
        m.release_at_once = true;
        m.releases = port;
    }
    let rig = Rig::with(RigConfig {
        flush: Some(w.site()),
        ..RigConfig::default()
    });
    let flushed = Arc::new(Flushed::default());
    let _ = flushed.v.set(rig.v.clone());
    rig.signals.register(flushed.clone());
    let a = branch(&rig.p, &rig.genesis, 2, 0);
    rig.accept(&a[0]);
    rig.sync();
    flushed.log.lock().expect("log").clear();
    let _adversary = adversarial(&rig);
    rig.accept(&a[1]);
    rig.sync();
    let (a1, a2) = (rig.id(&a[0]), rig.id(&a[1]));
    assert_eq!(
        *flushed.log.lock().expect("log"),
        [
            // AcceptBlock's NONE, outside a hold: queued at once, so it runs at the release,
            // before a2 is published.
            (a[0].hash(), a1),
            // ConnectTip's IF_NEEDED names a1, the tip before the step; staged, it follows the
            // release that published a2.
            (a[0].hash(), a2),
            // ABC's PERIODIC, outside the hold.
            (a[1].hash(), a2),
        ]
    );
    let locator = rig.v.reader().locator(Some(a1));
    assert_eq!(locator[0], a[0].hash(), "the reader's locator from the site's tip");
}

#[test]
fn chain_state_flushed_names_the_sites_tip_and_is_staged_in_a_hold() {
    chain_state_flushed_at(None);
}

/// The same through CH2b's `FlushSignals` port, as `FlushControl` releases: V's
/// [`FlushReleases`] hands the tips to the host, so they are staged too.
#[test]
fn chain_state_flushed_through_the_flush_signals_port_is_staged_in_a_hold() {
    chain_state_flushed_at(Some(FlushReleases::default()));
}

/// V's thread's CPU time so far, in clock ticks (`/proc/self/task/<tid>/stat`).
fn v_cpu_ticks(rig: &Rig) -> u64 {
    let (tx, rx) = reply();
    rig.on_v(move |_| {
        tx.send(std::fs::read_link("/proc/thread-self").expect("Linux"));
        Ok(())
    });
    let task = rx.recv().expect("V");
    let stat = std::fs::read_to_string(Path::new("/proc").join(task).join("stat")).expect("stat");
    let fields: Vec<&str> = stat.rsplit(')').next().expect("comm").split_whitespace().collect();
    // utime and stime, fields 14 and 15 of the whole line.
    fields[11].parse::<u64>().expect("utime") + fields[12].parse::<u64>().expect("stime")
}

/// BL-123: a W that has ended (its bell's sender gone) leaves V idle, not spinning on a
/// disconnected bell, which is always ready; V serves on.
#[test]
fn an_idle_v_whose_w_has_ended_does_not_spin() {
    let w = ModelW::default();
    let rig = Rig::with(RigConfig {
        flush: Some(w.site()),
        ..RigConfig::default()
    });
    w.lock().bell = None;
    let before = v_cpu_ticks(&rig);
    std::thread::sleep(Duration::from_secs(1));
    let ticks = v_cpu_ticks(&rig) - before;
    // A spinning V burns the whole second, 100 ticks at 100 Hz, and still about 16 at the box's
    // peak load (190 on 32 vCPUs).
    assert!(ticks <= 5, "V used {ticks} ticks idle");
}

/// D45: after V's lanes close (every handle gone), `limit_queue` still answers W until its
/// barrier.
#[test]
fn a_prune_flush_while_v_waits_with_its_lanes_closed_is_answered() {
    let w = ModelW::default();
    let rig = Rig::with(RigConfig {
        flush: Some(w.site()),
        ..RigConfig::default()
    });
    let go = held_signals(&rig);
    let (done, waited) = reply();
    let cmd = ChainCommand::TestOnV(Box::new(move |v| {
        v.forget_lanes();
        v.limit_queue()?;
        done.send(());
        Ok(())
    }));
    rig.v.send(cmd).expect("sent");
    assert_eq!(w.prune(&[], 0), Ok(ClearsAnswer::Declined));
    assert!(waited.try_recv().is_none(), "V still waits");
    drop(go);
    waited.recv_timeout(SOON).expect("the wait ends").expect("ran");
}

/// A listener that holds the signals thread on [`HELD`]'s tip until it is let go, so V's
/// `limit_queue` waits (more than 10 callbacks pending).
struct Holder(Mutex<mpsc::Receiver<()>>);

const HELD: u32 = 1_000;

impl ValidationInterface for Holder {
    fn updated_block_tip(&self, new: BlockId, _: Option<BlockId>, _: bool) {
        if new == BlockId(HELD) {
            let _ = self.0.lock().expect("gate").recv();
        }
    }
}

/// Lets a [`Holder`] go when dropped, so a failing test still lets the signals thread finish.
struct LetGo(mpsc::Sender<()>);

impl Drop for LetGo {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

/// V in `LimitValidationInterfaceQueue` (§9.6 P-2) inside a class C operation, and the wait's
/// end: the operation's result once the holder lets go.
fn v_in_limit_queue(rig: &Rig) -> (LetGo, ActorReply<()>) {
    let go = held_signals(rig);
    let (done, waited) = reply();
    let cmd = ChainCommand::TestOnV(Box::new(move |v| {
        v.limit_queue()?;
        done.send(());
        Ok(())
    }));
    rig.v.send(cmd).expect("sent");
    // A query is served only at the wait, so V is in it.
    rig.v.query(|_| ()).recv_timeout(SOON).expect("served at the wait").expect("served");
    (go, waited)
}

/// The signals thread held on [`HELD`]'s tip with 12 callbacks queued, so V's next
/// `limit_queue` waits until the returned [`LetGo`] drops.
fn held_signals(rig: &Rig) -> LetGo {
    let (go, gate) = mpsc::channel();
    rig.signals.register(Arc::new(Holder(Mutex::new(gate))));
    for i in 0..12 {
        rig.signals.enqueue(Queued::UpdatedBlockTip {
            new: BlockId(HELD + i),
            fork: None,
            ibd: false,
        });
    }
    LetGo(go)
}

/// BL-123 (D45 (c)): W's clears are answered while V waits in `limit_queue`, which keeps
/// waiting.
#[test]
fn a_prune_flush_while_v_waits_in_limit_queue_is_answered() {
    let w = ModelW::default();
    let rig = Rig::with(RigConfig {
        flush: Some(w.site()),
        ..RigConfig::default()
    });
    let (go, waited) = v_in_limit_queue(&rig);
    // At genesis no selection is allowed (`FindFilesToPrune`'s early return).
    assert_eq!(w.prune(&[], 0), Ok(ClearsAnswer::Declined));
    assert!(waited.try_recv().is_none(), "V still waits");
    drop(go);
    waited.recv_timeout(SOON).expect("the wait ends").expect("ran");
    rig.assert_no_abort();
}

/// D45: a failure V's flush side reports at a `limit_queue` wait stops V as the flush failing,
/// reported once, by W (L-3).
#[test]
fn a_failed_flush_service_while_v_waits_in_limit_queue_stops_v() {
    let w = ModelW::default();
    let mut rig = Rig::with(RigConfig {
        flush: Some(w.site()),
        ..RigConfig::default()
    });
    w.lock().sink = Some(rig.abort.clone());
    let (go, waited) = v_in_limit_queue(&rig);
    w.fail_job("the flush failed");
    assert_eq!(waited.recv_timeout(SOON), Some(Err(WaitError::Disconnected)), "V stopped");
    drop(go);
    // The reply drops with the closure, before V's run loop reaches the abort sink.
    let exit = rig.stop();
    assert_eq!(exit.fatal.map(|f| f.0).as_deref(), Some("the flush failed"));
    assert_eq!(rig.abort.messages(), ["the flush failed"]);
}

/// BL-123 (D45 (b)): a request W sends while V is busy is answered at V's next release point.
/// AcceptBlock's NONE site sends it, and ABC's first release point, before its hold connects
/// a1, answers it at genesis; the idle select would answer it at a1.
#[test]
fn a_prune_flush_is_answered_at_the_next_release_point() {
    let w = ModelW::default();
    let rig = Rig::with(RigConfig {
        flush: Some(w.site()),
        ..RigConfig::default()
    });
    let (r, answer) = ModelW::request(&[], 0, AUTO);
    w.lock().armed = Some((FlushStateMode::None, r));
    rig.accept(&branch(&rig.p, &rig.genesis, 1, 0)[0]);
    assert_eq!(answer.recv_timeout(SOON), Some(Ok(Ok(ClearsAnswer::Declined))));
    assert_eq!(w.lock().answered_at, [Some(0)]);
}

/// D47 item 6 (Sol N7a r1 R1-m-1): before init's ABC connects genesis, V has no tip, and a request
/// answered then is declined, as `FindFilesToPrune` returns early at `Height() == -1`; nothing
/// is cleared, so `m_have_pruned` stays unset.
#[test]
fn a_prune_flush_before_genesis_is_connected_is_declined() {
    let w = ModelW::default();
    let wm = w.clone();
    let (sent, answers) = mpsc::channel();
    let rig = Rig::with(RigConfig {
        flush: Some(w.site()),
        // V is idle here: the request is answered at its select, before init's ABC.
        before_init: Some(Box::new(move |_: &ChainHandle| {
            sent.send(wm.send(&[1], 0).recv_timeout(SOON)).expect("the test waits");
            // `pruneblockchain 0`: its rule allows block 0 at a made-up genesis tip (N7a r2 N-7).
            let manual = wm.send_under(&[0], 0, PruneRule::Manual(0));
            sent.send(manual.recv_timeout(SOON)).expect("the test waits");
        })),
        ..RigConfig::default()
    });
    for _ in 0..2 {
        assert_eq!(answers.recv(), Ok(Some(Ok(Ok(ClearsAnswer::Declined)))));
    }
    assert_eq!(w.lock().answered_at, [None, None]);
    assert!(!rig.v.query(|q| q.have_pruned).recv().expect("V serves"));
    rig.assert_no_abort();
}

/// D47 item 6 (R1-m-1): the devnet step's AcceptBlock is a NONE site with no tip (D48). A request W
/// sends there is declined at V's next wait, which comes before init's ABC connects genesis.
#[test]
fn a_prune_flush_sent_at_the_devnet_step_is_declined() {
    let w = ModelW::default();
    let (r, answer) = ModelW::request(&[1], 0, AUTO);
    w.lock().armed = Some((FlushStateMode::None, r));
    let rig = Rig::with(RigConfig {
        params: Some(devnet()),
        flush: Some(w.site()),
        no_check_block_index: true,
        ..RigConfig::default()
    });
    assert_eq!(answer.recv_timeout(SOON), Some(Ok(Ok(ClearsAnswer::Declined))));
    let m = w.lock();
    assert_eq!(m.answered_at, [None]);
    assert_eq!(m.sites.first().map(|s| (s.mode, s.at)), Some((FlushStateMode::None, None)));
    drop(m);
    rig.assert_no_abort();
}

/// D45's stop order: a request still pending when V stops is answered by V's last service,
/// before W is dropped.
#[test]
fn a_prune_flush_pending_when_v_stops_is_answered() {
    let w = ModelW::default();
    let mut rig = Rig::with(RigConfig {
        flush: Some(w.site()),
        ..RigConfig::default()
    });
    // Sent without a ring: nothing wakes V for it before the stop.
    let (r, answer) = ModelW::request(&[], 0, AUTO);
    w.lock().requests.push_back(r);
    assert!(rig.stop().fatal.is_none());
    assert_eq!(answer.recv_timeout(SOON), Some(Ok(Ok(ClearsAnswer::Declined))));
}

/// D45 (M-1): W selects a prune after V has taken `Shutdown`, its job still in flight. V's stop
/// waits for the release (`FlushSite::stop`) and answers the clears there, so the prune applies
/// and the node stops without `AbortNode`, as Core's shutdown flush prunes synchronously.
#[test]
fn a_prune_flush_w_sends_after_shutdown_is_taken_is_applied() {
    let w = ModelW::default();
    let (stopping, stop_began) = reply();
    {
        let mut m = w.lock();
        m.in_flight = true;
        m.stopping = Some(stopping);
    }
    let mut rig = Rig::with(RigConfig {
        flush: Some(w.site()),
        ..RigConfig::default()
    });
    let base = branch(&rig.p, &rig.genesis, 290, 0);
    for m in &base {
        rig.accept(m);
    }
    let (_, stale_ids) = stale_in_file_1(&rig);
    rig.v.shutdown();
    stop_began.recv_timeout(SOON).expect("V stops").expect("its stop waits");
    let answer = w.prune(&[1], 2);
    w.release();
    let mut exit = rig.stop();
    assert!(exit.fatal.is_none() && exit.have_pruned);
    let ix = exit.chain.node_index().expect("the node's chain");
    let cleared = stale_ids.iter().map(|&id| ix.record(id)).collect();
    assert_eq!(answer, Ok(ClearsAnswer::Cleared(cleared)));
    for &id in &stale_ids {
        assert!(!exit.chain.reader().entry(id).expect("held").status.has(Status::HAVE_DATA));
    }
    assert_eq!(w.lock().stops, 1);
    rig.assert_no_abort();
}

/// D45 (M-1): W's job in flight fails while V's stop waits for it. The latch trips, `stop`
/// returns the `Fatal`, and V takes its `Fatal` path: one `AbortNode`, W's (L-3), and W runs
/// none of its queued jobs. A stop that returns its failure is no panic: init's command is still
/// served (CH7-notes D72, Sol BL-547 r2), as Core's `loadblk` calls it after `AbortNode`.
// Core: init.cpp:2458-2473
#[test]
fn a_job_failing_while_vs_stop_waits_takes_the_fatal_path() {
    let abort = Arc::new(RecordingAbort::default());
    let w = w_with_jobs(&abort);
    let (stopping, stop_began) = reply();
    w.lock().stopping = Some(stopping);
    let calls = Arc::new(InitCalls::default());
    let mut rig = Rig::with(RigConfig {
        flush: Some(w.site()),
        abort: Some(abort.clone()),
        listener: Some(calls.clone()),
        ..RigConfig::default()
    });
    rig.v.shutdown();
    stop_began.recv_timeout(SOON).expect("V stops").expect("its stop waits");
    w.fail_job("Failed to write to block index database");
    let exit = rig.stop();
    assert_eq!(exit.fatal.map(|f| f.0).as_deref(), Some("Failed to write to block index database"));
    assert_eq!(abort.messages(), ["Failed to write to block index database"]);
    assert_eq!(w.end(), 0, "no queued job runs");
    assert_eq!(w.lock().stops, 1);
    rig.v
        .send(ChainCommand::SetLoading {
            importing: Some(false),
            reindex: None,
        })
        .expect("the reaper's lane");
    let moves = rig.signals.dmn_moves();
    assert_eq!(rig.v.initialize_current_block_tip().recv_timeout(SOON), Some(Ok(())));
    assert_eq!(rig.signals.dmn_moves(), moves + 1);
    assert_eq!(rig.signals.dmn_tip(), Some(rig.id(&rig.genesis)));
    assert_eq!(calls.0.load(Ordering::SeqCst), 1);
}

/// D62 and D72: an `AbortNode` that is not the store's, then W's job fails while V's stop waits
/// for the release. The stop returns W's failure, which W has reported (L-3), and the store
/// stays shut; init is still served once importing clears, as Core's `loadblk` calls
/// `InitializeCurrentBlockTip` after `AbortNode` (`init.cpp:2458-2473`).
#[test]
fn a_job_failing_while_vs_stop_after_an_abort_waits_still_serves_init() {
    let abort = Arc::new(RecordingAbort::default());
    let w = w_with_jobs(&abort);
    let (stopping, stop_began) = reply();
    w.lock().stopping = Some(stopping);
    let calls = Arc::new(InitCalls::default());
    let mut rig = Rig::with(RigConfig {
        flush: Some(w.site()),
        abort: Some(abort.clone()),
        listener: Some(calls.clone()),
        ..RigConfig::default()
    });
    let fail = ChainCommand::TestOnV(Box::new(|_| Err(Fatal::new("Failed to read block"))));
    rig.v.send(fail).expect("sent");
    stop_began.recv_timeout(SOON).expect("V stops its flush side").expect("its stop waits");
    w.fail_job("Failed to write to block index database");
    assert_eq!(rig.stop().fatal.map(|f| f.0).as_deref(), Some("Failed to read block"));
    assert_eq!(
        abort.messages(),
        ["Failed to read block", "Failed to write to block index database"]
    );
    assert_eq!(w.end(), 0, "no queued job runs");
    let m = w.lock();
    assert_eq!(m.aborts, [AbortCause::AbortNode]);
    assert_eq!((m.latched, m.stops), (true, 1));
    drop(m);
    assert_eq!(w.shutdown_flush(), Err(FLUSH_REFUSED));
    rig.v
        .send(ChainCommand::SetLoading {
            importing: Some(false),
            reindex: None,
        })
        .expect("the reaper's lane");
    let moves = rig.signals.dmn_moves();
    assert_eq!(rig.v.initialize_current_block_tip().recv_timeout(SOON), Some(Ok(())));
    assert_eq!(rig.signals.dmn_moves(), moves + 1);
    assert_eq!(rig.signals.dmn_tip(), Some(rig.id(&rig.genesis)));
    assert_eq!(calls.0.load(Ordering::SeqCst), 1);
}

/// D45 (L-3): W's job fails on W's thread while V waits on nothing. W reports it there, as
/// CH2b's driver does; V hears it at its idle select's bell and stops, and its sink stays
/// silent: one `AbortNode`, as Core's failing `FlushStateToDisk` reports once
/// (`validation.cpp:3066`).
#[test]
fn a_w_failure_while_v_is_idle_is_reported_once() {
    let abort = Arc::new(RecordingAbort::default());
    let w = w_with_jobs(&abort);
    let mut rig = Rig::with(RigConfig {
        flush: Some(w.site()),
        abort: Some(abort.clone()),
        ..RigConfig::default()
    });
    // V has started and is idle in its select.
    rig.v.query(|_| ()).recv_timeout(SOON).expect("V serves").expect("served");
    w.fail_job("Failed to write to block index database");
    let exit = rig.stop();
    assert_eq!(exit.fatal.map(|f| f.0).as_deref(), Some("Failed to write to block index database"));
    assert_eq!(abort.messages(), ["Failed to write to block index database"]);
    assert_eq!(w.end(), 0, "no queued job runs");
    assert_eq!(w.lock().stops, 0, "an AbortNode stop");
}

/// A model W with a job in flight, two queued behind it, and CH2b's sink (`abort`), with V's.
fn w_with_jobs(abort: &Arc<RecordingAbort>) -> ModelW {
    let w = ModelW::default();
    {
        let mut m = w.lock();
        m.in_flight = true;
        m.queued_jobs = 2;
        m.sink = Some(abort.clone());
    }
    w
}

/// D62 (BL-202, DEC-241): an `AbortNode` that is not the store's, with W's job in flight, two
/// queued and its clears pending. The store stays open: V's sink reports, then its flush side
/// stops in D45's order, answering the clears and waiting for the release; W's end runs the
/// queued jobs, and a shutdown flush afterwards is not refused. One `AbortNode`, V's.
#[test]
fn an_abort_leaves_the_store_open_for_the_shutdown_flushes() {
    let abort = Arc::new(RecordingAbort::default());
    let w = w_with_jobs(&abort);
    let (stopping, stop_began) = reply();
    w.lock().stopping = Some(stopping);
    let mut rig = Rig::with(RigConfig {
        flush: Some(w.site()),
        abort: Some(abort.clone()),
        ..RigConfig::default()
    });
    // W's step 7, sent but not yet answered.
    let (r, pending) = ModelW::request(&[], 0, AUTO);
    w.lock().requests.push_back(r);
    let fail = ChainCommand::TestOnV(Box::new(|_| Err(Fatal::new("Failed to read block"))));
    rig.v.send(fail).expect("sent");
    stop_began.recv_timeout(SOON).expect("V stops its flush side").expect("its stop waits");
    // Read before the release, asserted after it, so a failure does not leave V waiting.
    let sunk = abort.messages();
    w.release();
    assert_eq!(sunk, ["Failed to read block"], "the sink came first");
    assert_eq!(pending.recv_timeout(SOON), Some(Ok(Ok(ClearsAnswer::Declined))));
    assert_eq!(rig.stop().fatal.map(|f| f.0).as_deref(), Some("Failed to read block"));
    assert_eq!(w.end(), 2, "the queued jobs run");
    assert_eq!(w.shutdown_flush(), Ok(()), "no refused flush");
    assert_eq!(abort.messages(), ["Failed to read block"]);
    let m = w.lock();
    assert_eq!(m.aborts, [AbortCause::AbortNode]);
    assert_eq!((m.latched, m.stops), (false, 1));
}

/// D62 (BL-202): V's `AbortNode` for a failed store call. The guard has tripped the latch and
/// reported, so V's sink stays silent (L-3), V does not stop its flush side, W runs none of its
/// queued jobs, and the shutdown flushes are refused, as Core's are after a failed write.
#[test]
fn a_store_failure_keeps_the_latch_tripped_for_the_shutdown_flushes() {
    const WRITE_FAILED: &str = "Failed to write to block index database";
    let abort = Arc::new(RecordingAbort::default());
    let w = w_with_jobs(&abort);
    let mut rig = Rig::with(RigConfig {
        flush: Some(w.site()),
        abort: Some(abort.clone()),
        ..RigConfig::default()
    });
    let store = w.clone();
    let fail = ChainCommand::TestOnV(Box::new(move |_| {
        store.store_fails(WRITE_FAILED);
        Err(Fatal::new(WRITE_FAILED))
    }));
    rig.v.send(fail).expect("sent");
    assert_eq!(rig.stop().fatal.map(|f| f.0).as_deref(), Some(WRITE_FAILED));
    assert_eq!(abort.messages(), [WRITE_FAILED], "the guard's report only");
    assert_eq!(w.end(), 0, "no queued job runs");
    assert_eq!(w.shutdown_flush(), Err(FLUSH_REFUSED));
    let m = w.lock();
    assert_eq!((m.aborts.as_slice(), m.stops), ([AbortCause::AbortNode].as_slice(), 0));
}

/// Counts init's `InitializeCurrentBlockTip` calls.
#[derive(Default)]
pub(super) struct InitCalls(AtomicUsize);

impl ValidationInterface for InitCalls {
    fn initialize_current_block_tip(&self, _: Option<BlockId>, _: bool) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// CH7-notes D72 after V's panic in the flush side's stop: init's command, sent once V is gone,
/// disconnects, the DMN tip stays and no listener runs.
pub(super) fn assert_init_dropped(rig: &Rig, calls: &InitCalls) {
    rig.v
        .send(ChainCommand::SetLoading {
            importing: Some(false),
            reindex: None,
        })
        .expect("the reaper's lane");
    let moves = rig.signals.dmn_moves();
    assert_eq!(
        rig.v.initialize_current_block_tip().recv_timeout(SOON),
        Some(Err(WaitError::Disconnected))
    );
    assert_eq!(rig.signals.dmn_moves(), moves);
    assert_eq!(calls.0.load(Ordering::SeqCst), 0);
}

/// D62 (validator r1): the flush side's stop after a non-store `AbortNode` panics. That is V's
/// panic, outside `serve`'s `catch_unwind`: the latch trips, and V's sink, which has reported,
/// stays silent, so the shutdown flushes are refused. Init's command is then dropped (D72).
#[test]
fn a_panic_in_the_flush_stop_after_an_abort_trips_the_latch() {
    let abort = Arc::new(RecordingAbort::default());
    let w = w_with_jobs(&abort);
    w.lock().stop_panics = true;
    let calls = Arc::new(InitCalls::default());
    let mut rig = Rig::with(RigConfig {
        flush: Some(w.site()),
        abort: Some(abort.clone()),
        listener: Some(calls.clone()),
        ..RigConfig::default()
    });
    let fail = ChainCommand::TestOnV(Box::new(|_| Err(Fatal::new("Failed to read block"))));
    rig.v.send(fail).expect("sent");
    let joined = rig.join.take().expect("running").join().expect("an unmarked joiner");
    assert!(joined.is_err(), "the panic resumed");
    let m = w.lock();
    assert_eq!(m.aborts, [AbortCause::AbortNode, AbortCause::Panic]);
    assert_eq!(m.latched_before_sink, Some(false), "after V's sink");
    drop(m);
    assert_eq!(abort.messages(), ["Failed to read block"]);
    assert_eq!(w.shutdown_flush(), Err(FLUSH_REFUSED));
    assert_init_dropped(&rig, &calls);
}

/// D62 (validator r1; Opus D55 r1 n-2): the flush side's stop on V's orderly stop panics. V
/// reports it as its panic and stops serving, the latch trips, and the node's flag is set.
/// Init's command is then dropped (D72).
#[test]
fn a_panic_in_the_orderly_flush_stop_is_vs_panic() {
    let abort = Arc::new(RecordingAbort::default());
    let w = ModelW::default();
    {
        let mut m = w.lock();
        m.sink = Some(abort.clone());
        m.stop_panics = true;
    }
    let calls = Arc::new(InitCalls::default());
    let mut rig = Rig::with(RigConfig {
        flush: Some(w.site()),
        abort: Some(abort.clone()),
        listener: Some(calls.clone()),
        ..RigConfig::default()
    });
    rig.v.shutdown();
    let joined = rig.join.take().expect("running").join().expect("an unmarked joiner");
    assert!(joined.is_err(), "the panic resumed");
    let m = w.lock();
    assert_eq!(
        (m.aborts.as_slice(), m.latched_before_sink),
        ([AbortCause::Panic].as_slice(), Some(true))
    );
    drop(m);
    assert_eq!(abort.messages(), ["V panicked: W's stop panicked"]);
    assert!(rig.shutdown.shutdown_requested(), "the node's flag");
    assert_init_dropped(&rig, &calls);
}

/// D62a: `CheckBlockIndex` is assert-only in Core (`v:5755`, its candidate rule here): V's port
/// stops V as Core's assert would, the store latch tripped and no shutdown flush.
#[test]
fn a_check_block_index_failure_stops_v_as_cores_assert() {
    let w = ModelW::default();
    let mut rig = Rig::with(RigConfig {
        flush: Some(w.site()),
        ..RigConfig::default()
    });
    let fail = ChainCommand::TestOnV(Box::new(|v| {
        let tip = v.tip().expect("genesis is connected");
        v.idx.candidates_erase(tip);
        v.check_block_index()
    }));
    rig.v.send(fail).expect("sent");
    let fatal = rig.stop().fatal.expect("V aborted").0;
    assert!(fatal.starts_with("CheckBlockIndex: "), "{fatal}");
    assert_eq!(rig.abort.messages(), [fatal]);
    w.assert_stopped_as_by_an_assert();
}

/// D62a: V's invariants C1 and M1, checked after every command in debug builds, are V's
/// asserts: a broken C1 (a candidate left in `cand_key` only) or M1 (the mirror's
/// `enabled_observed` off SB4's) stops V as Core's assert.
#[cfg(debug_assertions)]
#[test]
fn an_invariant_failure_stops_v_as_cores_assert() {
    type Break = fn(&mut crate::chain::actor::V);
    let breaks: [(&str, Break); 2] = [
        ("C1: ", |v| v.idx.break_c1()),
        ("M1: ", |v| {
            let mut sigs = (*v.cl.sigs).clone();
            sigs.enabled_observed = !sigs.enabled_observed;
            v.cl.sigs = Arc::new(sigs);
        }),
    ];
    for (invariant, break_it) in breaks {
        let w = ModelW::default();
        let mut rig = Rig::with(RigConfig {
            flush: Some(w.site()),
            ..RigConfig::default()
        });
        let fail = ChainCommand::TestOnV(Box::new(move |v| {
            break_it(v);
            Ok(())
        }));
        rig.v.send(fail).expect("sent");
        let fatal = rig.stop().fatal.expect("V aborted").0;
        assert!(fatal.starts_with(invariant), "{fatal}");
        assert_eq!(rig.abort.messages(), [fatal]);
        w.assert_stopped_as_by_an_assert();
    }
}

/// D45 (L-2): V's panic trips the latch before its sink too.
#[test]
fn a_panic_trips_the_store_latch_before_the_sink() {
    let abort = Arc::new(RecordingAbort::default());
    let w = w_with_jobs(&abort);
    let rig = Rig::with(RigConfig {
        flush: Some(w.site()),
        abort: Some(abort.clone()),
        ..RigConfig::default()
    });
    rig.v.send(ChainCommand::TestOnV(Box::new(|_| panic!("boom")))).expect("sent");
    // V's stop disconnects the queued replies after its sink.
    assert_eq!(rig.v.query(|_| ()).recv_timeout(SOON), Some(Err(WaitError::Disconnected)));
    assert_eq!(abort.messages(), ["V panicked: boom"]);
    assert_eq!(w.lock().latched_before_sink, Some(true));
    assert_eq!(w.end(), 0, "no queued job runs");
    assert_eq!(w.shutdown_flush(), Err(FLUSH_REFUSED), "as Core's assert, no flush");
}

/// `FlushHost` as CH2b's `FlushIndex`: `take_dirty` hands W the block index's records B1 writes,
/// each entry's latest.
#[test]
fn the_host_hands_w_the_dirty_index_records() {
    let w = ModelW::default();
    w.lock().take_dirty = true;
    let rig = Rig::with(RigConfig {
        flush: Some(w.site()),
        ..RigConfig::default()
    });
    let a = branch(&rig.p, &rig.genesis, 1, 0);
    rig.accept(&a[0]);
    let dirty = std::mem::take(&mut w.lock().dirty);
    let last = dirty.iter().rev().find(|r| r.hash == a[0].hash());
    assert_eq!(last, records(&rig, &[rig.id(&a[0])]).first());
}

/// [`NullConnector`], counting its `ConnectBlock`s: the attached connector is V's (D51).
#[derive(Clone, Default)]
pub(super) struct Counts(Arc<AtomicUsize>);

impl Counts {
    pub(super) fn get(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

impl BlockConnector for Counts {
    fn connect_block(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        target: ViewTarget<'_>,
        block: &Block,
        checked: bool,
        id: BlockId,
        st: &mut BlockStatusWriter<'_>,
    ) -> Result<ConnectOutcome, Fatal> {
        self.0.fetch_add(1, Ordering::SeqCst);
        NullConnector.connect_block(cx, v, target, block, checked, id, st)
    }

    fn disconnect_block(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        target: ViewTarget<'_>,
        block: &Block,
        id: BlockId,
    ) -> Result<DisconnectResult, Fatal> {
        NullConnector.disconnect_block(cx, v, target, block, id)
    }

    fn test_block_validity(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        checks: &dyn ProposalChecks,
        block: &Block,
        prev: BlockId,
        pow: bool,
        merkle: bool,
    ) -> Result<ConnectOutcome, Fatal> {
        NullConnector.test_block_validity(cx, v, checks, block, prev, pow, merkle)
    }
}

/// ND1's coins side for [`ChainHandle::attach_coins`]: `connector` and the model W.
pub(super) fn coins(w: &ModelW, connector: &Counts) -> AttachCoins {
    AttachCoins {
        connector: Box::new(connector.clone()),
        flush: w.site(),
    }
}

/// A command V fails with [`COINS_ATTACH_MISUSE`] (D51): a coins attach to a V that has run a
/// class C operation. A test's way to raise an `AbortNode` from a served command.
pub(super) fn misused_attach(v: &ChainHandle) -> ActorReply<()> {
    v.attach_coins(coins(&ModelW::default(), &Counts::default()))
}

/// [`RigConfig::before_init`] attaching each of `coins` in turn and handing the test each reply.
fn attach_before_init(
    coins: impl IntoIterator<Item = AttachCoins> + 'static,
) -> (BeforeInit, mpsc::Receiver<Option<Result<(), WaitError>>>) {
    let (sent, attached) = mpsc::channel();
    let send = Box::new(move |v: &ChainHandle| {
        for c in coins {
            sent.send(v.attach_coins(c).recv_timeout(SOON)).expect("the test waits");
        }
    });
    (send, attached)
}

/// D51: the devnet step runs on the spawn's `NoFlush`, as Core's `LoadGenesisBlock` runs while
/// `CanFlushToDisk()` is false (`validation.cpp:4853-4855`): the W attached after `start` sees
/// no NONE site, and its first is genesis's ConnectTip with no tip (D47 item 6). The attached
/// connector connects both blocks.
#[test]
fn the_devnet_step_runs_before_the_coins_attach() {
    let (w, connects) = (ModelW::default(), Counts::default());
    let (send, attached) = attach_before_init([coins(&w, &connects)]);
    let rig = Rig::with(RigConfig {
        params: Some(devnet()),
        no_check_block_index: true,
        before_init: Some(send),
        ..RigConfig::default()
    });
    assert_eq!(attached.recv(), Ok(Some(Ok(()))));
    let sites = w.sites();
    assert!(sites.iter().all(|s| s.mode != FlushStateMode::None), "{sites:?}");
    assert_eq!(sites.first().map(|s| (s.mode, s.at)), Some((FlushStateMode::IfNeeded, None)));
    assert_eq!(connects.get(), 2);
    rig.assert_no_abort();
}

/// D51: the attached side is V's: its connector connects V's blocks, and W's bell rings V,
/// whose idle select answers the clears.
#[test]
fn the_attached_coins_connect_and_ring_v() {
    let (w, connects) = (ModelW::default(), Counts::default());
    let (send, attached) = attach_before_init([coins(&w, &connects)]);
    let rig = Rig::with(RigConfig {
        before_init: Some(send),
        ..RigConfig::default()
    });
    assert_eq!(attached.recv(), Ok(Some(Ok(()))));
    rig.accept(&branch(&rig.p, &rig.genesis, 1, 0)[0]);
    assert_eq!(connects.get(), 2);
    assert!(matches!(w.send(&[], 0).recv_timeout(SOON), Some(Ok(Ok(_)))));
    rig.assert_no_abort();
}

/// D51 (validator L-2): the spawn's side stops in D45's order before the attach replaces it,
/// and V services the attached side at once, so a request W queued before it had a bell is
/// answered by the time the attach is (declined: no tip yet, D47 item 6). The attach is not one
/// of D45's waits: neither side's `FlushDue` is answered there (D58).
#[test]
fn the_attach_stops_the_spawns_side_and_services_the_new_one() {
    let (spawned, w) = (ModelW::default(), ModelW::default());
    spawned.lock().due = true;
    let (r, answer) = ModelW::request(&[1], 0, AUTO);
    {
        let mut m = w.lock();
        m.requests.push_back(r);
        m.due = true;
    }
    let (attach, wm) = (coins(&w, &Counts::default()), w.clone());
    let (sent, replies) = mpsc::channel();
    let rig = Rig::with(RigConfig {
        flush: Some(spawned.site()),
        before_init: Some(Box::new(move |v: &ChainHandle| {
            let attached = v.attach_coins(attach).recv_timeout(SOON);
            // Before init's ABC, whose genesis decision takes the `FlushDue`.
            let dues = wm.lock().dues.len();
            sent.send((attached, answer.try_recv(), dues)).expect("the test waits");
        })),
        ..RigConfig::default()
    });
    let (attached, answered, dues) = replies.recv().expect("sent before init's ABC");
    assert_eq!(attached, Some(Ok(())));
    assert_eq!(answered, Some(Ok(Ok(ClearsAnswer::Declined))));
    assert_eq!(dues, 0, "the attached side's FlushDue was not answered at the attach");
    let m = spawned.lock();
    assert_eq!((m.services, m.stops), (1, 1), "the spawn's side was serviced, then stopped");
    assert!(m.due && m.dues.is_empty(), "the spawn's FlushDue was not answered");
    drop(m);
    rig.assert_no_abort();
}

/// D51: a second attach, even before init's ABC, is an invariant violation with no Core
/// analogue: one `AbortNode`, its reply disconnects, and the first side stays V's. Neither
/// store latch trips (D62): the second side drops unstopped, the first stops in order.
#[test]
fn a_second_coins_attach_aborts_the_node() {
    let (w, second) = (ModelW::default(), ModelW::default());
    let (send, replies) =
        attach_before_init([coins(&w, &Counts::default()), coins(&second, &Counts::default())]);
    let mut rig = Rig::with(RigConfig {
        before_init: Some(send),
        aborts_before_init: true,
        ..RigConfig::default()
    });
    let replies: Vec<_> = replies.iter().collect();
    assert_eq!(replies, [Some(Ok(())), Some(Err(WaitError::Disconnected))]);
    assert_eq!(rig.stop().fatal.map(|f| f.0).as_deref(), Some(COINS_ATTACH_MISUSE));
    assert_eq!(rig.abort.messages(), [COINS_ATTACH_MISUSE]);
    let refused = second.lock();
    assert!(refused.bell.is_none() && refused.aborts.is_empty() && refused.stops == 0);
    drop(refused);
    assert_eq!(w.lock().aborts, [AbortCause::AbortNode]);
    assert_eq!(w.lock().stops, 1);
    for side in [&w, &second] {
        assert_eq!(side.shutdown_flush(), Ok(()), "no refused flush");
    }
}

/// D51 (Opus r1 m-1): when the spawn's side fails to stop, the attach is that side's `Fatal`:
/// one `AbortNode`, the side's own (L-3), and the incoming side, which never became V's, drops
/// unstopped with its store open (D62).
#[test]
fn a_failed_stop_of_the_spawns_side_drops_the_attached_side() {
    const STOP_FAILED: &str = "W's last job failed";
    let (spawned, w) = (ModelW::default(), ModelW::default());
    let abort = Arc::new(RecordingAbort::default());
    {
        let mut m = spawned.lock();
        m.stop_fails = Some(STOP_FAILED);
        m.sink = Some(abort.clone());
    }
    let (send, replies) = attach_before_init([coins(&w, &Counts::default())]);
    let mut rig = Rig::with(RigConfig {
        flush: Some(spawned.site()),
        abort: Some(abort.clone()),
        before_init: Some(send),
        aborts_before_init: true,
        ..RigConfig::default()
    });
    assert_eq!(replies.recv(), Ok(Some(Err(WaitError::Disconnected))));
    assert_eq!(rig.stop().fatal.map(|f| f.0).as_deref(), Some(STOP_FAILED));
    assert_eq!(abort.messages(), [STOP_FAILED]);
    let refused = w.lock();
    assert!(refused.bell.is_none() && refused.aborts.is_empty() && !refused.latched);
    drop(refused);
    let old = spawned.lock();
    assert_eq!(old.stops, 1, "no second stop on a tripped latch");
    assert_eq!(old.aborts, [AbortCause::AbortNode]);
}

/// BL-271 (Sol r1): a shutdown flush on V's exit failing with no report made and the latch clear
/// is reported there, as V's stop reports one (L-3), before it returns: Core's `AbortNode`
/// precedes ND1's "failed to flush state" line. The flush side stays, so the second flush runs.
// Core: validation.cpp:2245-2249, :3128-3133
#[test]
fn an_unreported_shutdown_flush_failure_is_reported_before_it_returns() {
    const FAILED: &str = "a shutdown flush failed";
    let w = ModelW::default();
    let mut rig = Rig::with(RigConfig {
        flush: Some(w.site()),
        ..RigConfig::default()
    });
    let mut exit = rig.stop();
    assert!(exit.fatal.is_none());
    w.lock().flush_fails = Some(FAILED);
    let failed = exit.force_flush(ShutdownFlush::First).map_err(|f| f.0);
    assert_eq!(failed, Err(FAILED.to_owned()));
    assert_eq!(rig.abort.messages(), [FAILED]);
    assert_eq!(w.lock().aborts, [AbortCause::AbortNode], "a latch read");
    let sites = w.sites().len();
    assert_eq!(exit.force_flush(ShutdownFlush::Second), Ok(()));
    assert_eq!(w.sites().len(), sites + 1, "the second flush ran");
    assert_eq!(rig.abort.messages(), [FAILED]);
}

/// D51 (validator L-4): `composite` closes the attach before its operation runs, so an attach V
/// takes at a release point inside its first class C operation is refused.
#[test]
fn a_coins_attach_inside_the_first_class_c_operation_aborts_the_node() {
    let w = ModelW::default();
    let attach = coins(&w, &Counts::default());
    let (sent, replies) = mpsc::channel();
    let mut rig = Rig::with(RigConfig {
        before_init: Some(Box::new(move |v: &ChainHandle| {
            let h = v.clone();
            let on_v = ChainCommand::TestOnV(Box::new(move |vv| {
                sent.send(h.attach_coins(attach)).expect("the test waits");
                vv.release_point()
            }));
            v.send(on_v).expect("V runs");
        })),
        aborts_before_init: true,
        ..RigConfig::default()
    });
    let reply = replies.recv_timeout(SOON).expect("sent on V");
    assert_eq!(reply.recv(), Err(WaitError::Disconnected));
    assert_eq!(rig.stop().fatal.map(|f| f.0).as_deref(), Some(COINS_ATTACH_MISUSE));
    assert_eq!(rig.abort.messages(), [COINS_ATTACH_MISUSE]);
    assert!(w.lock().bell.is_none(), "W was not attached");
}

/// D51: an attach after a class C operation (here init's ABC, which ran on the spawn's side) is
/// one `AbortNode`.
#[test]
fn a_coins_attach_after_init_abc_aborts_the_node() {
    let mut rig = Rig::new();
    let w = ModelW::default();
    let reply = rig.v.attach_coins(coins(&w, &Counts::default())).recv();
    assert_eq!(reply, Err(WaitError::Disconnected));
    assert_eq!(rig.stop().fatal.map(|f| f.0).as_deref(), Some(COINS_ATTACH_MISUSE));
    assert_eq!(rig.abort.messages(), [COINS_ATTACH_MISUSE]);
    assert!(w.lock().bell.is_none(), "W was not attached");
}

/// V's tip and the last seq: the post-step pair of the latest site.
fn last_pair(w: &ModelW) -> (BlockRef, u64) {
    w.sites().last().and_then(|s| s.after).expect("a site with a tip")
}

/// D47 item 10 at D45 (a): a subscriber's `FlushDue` raised while V is idle is answered at its
/// select, one freeze at `(V's tip, the last seq)`.
#[test]
fn a_flush_due_is_answered_at_vs_idle_select() {
    let w = ModelW::default();
    let rig = Rig::with(RigConfig {
        flush: Some(w.site()),
        ..RigConfig::default()
    });
    rig.accept(&branch(&rig.p, &rig.genesis, 1, 0)[0]);
    let at = last_pair(&w);
    assert_eq!(at.0.height, 1);
    w.flush_due();
    assert_eq!(w.await_due_freezes(1), [at]);
    assert_eq!(w.lock().dues, [Some(1)]);
    rig.assert_no_abort();
}

/// D47 item 10 at D45 (b): a `FlushDue` raised at AcceptBlock's NONE site is answered at ABC's
/// first release point, at genesis, before its hold connects a1.
#[test]
fn a_flush_due_is_answered_at_the_next_release_point() {
    let w = ModelW::default();
    let rig = Rig::with(RigConfig {
        flush: Some(w.site()),
        ..RigConfig::default()
    });
    let before = w.sites().len();
    w.lock().due_armed = Some(FlushStateMode::None);
    rig.accept(&branch(&rig.p, &rig.genesis, 1, 0)[0]);
    let none = w.sites()[before];
    assert_eq!(none.mode, FlushStateMode::None);
    assert_eq!(w.await_due_freezes(1), [none.at.expect("genesis")]);
    assert_eq!(w.lock().dues, [Some(0)]);
    rig.assert_no_abort();
}

/// D47 item 10 at D45 (c): a `FlushDue` is answered while V waits in `limit_queue`, which keeps
/// waiting. It is raised as V enters the wait and no command reaches V meanwhile, so no release
/// point (b) can answer it first.
#[test]
fn a_flush_due_while_v_waits_in_limit_queue_is_answered() {
    let w = ModelW::default();
    let rig = Rig::with(RigConfig {
        flush: Some(w.site()),
        ..RigConfig::default()
    });
    let at = last_pair(&w);
    let go = held_signals(&rig);
    let (done, waited) = reply();
    let wv = w.clone();
    let cmd = ChainCommand::TestOnV(Box::new(move |v| {
        wv.flush_due();
        v.limit_queue()?;
        done.send(());
        Ok(())
    }));
    rig.v.send(cmd).expect("sent");
    assert_eq!(w.await_due_freezes(1), [at]);
    assert!(waited.try_recv().is_none(), "V still waits");
    drop(go);
    waited.recv_timeout(SOON).expect("the wait ends").expect("ran");
    rig.assert_no_abort();
}

/// D47 item 10 at D45 (d), the X15 wait: a `FlushDue` raised while V's hold waits in its drain
/// for that subscriber is answered there, at the hold's stepped tip and its seq, before the
/// release publishes it. Without the answer a Synced subscriber parked at its limit never frees
/// its queue.
#[test]
fn a_flush_due_during_a_hold_is_answered_at_the_stepped_tip() {
    let w = ModelW::default();
    let rig = Rig::with(RigConfig {
        feed_capacity: Some(1),
        flush: Some(w.site()),
        ..RigConfig::default()
    });
    let main = branch(&rig.p, &rig.genesis, 2, 0);
    let side = branch(&rig.p, &rig.genesis, 3, 1);
    for m in main.iter().chain(&side[..2]) {
        rig.accept(m);
    }
    let before = w.sites().len();
    // Two disconnects and three connects in one hold; the subscriber holds the second step's
    // event, so V waits in that step's drain at genesis.
    let (stepped, go, done) = gated_subscriber(&rig, 5);
    let w_thread = w_in_hold(&w, &rig, stepped, go, rig.id(&main[1]), |w| {
        // V's first answer in the drain: it is parked there, past step 2's decision.
        drop(w.wait_for(|m| !m.dues.is_empty()));
        w.flush_due();
        w.await_due_freezes(1)
    });
    rig.accept(&side[2]);
    let freezes = w_thread();
    assert_eq!(done().len(), 5);
    let sites = w.sites().split_off(before);
    let second = sites[2].after.expect("the second step's pair");
    assert_eq!(
        (sites[2].mode, second.0),
        (FlushStateMode::IfNeeded, block_ref(&rig, &rig.genesis))
    );
    assert_eq!(freezes, [second]);
    assert_eq!(w.lock().dues.first(), Some(&Some(0)));
    rig.assert_no_abort();
}

/// [`NullConnector`] raising a subscriber's `FlushDue` with a ring on its next connect, as W's
/// hub may while the connector commits.
struct DueOnCommit {
    w: ModelW,
    armed: Arc<AtomicBool>,
}

impl BlockConnector for DueOnCommit {
    fn connect_block(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        target: ViewTarget<'_>,
        block: &Block,
        checked: bool,
        id: BlockId,
        st: &mut BlockStatusWriter<'_>,
    ) -> Result<ConnectOutcome, Fatal> {
        if self.armed.swap(false, Ordering::SeqCst) {
            self.w.flush_due();
        }
        NullConnector.connect_block(cx, v, target, block, checked, id, st)
    }

    fn disconnect_block(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        target: ViewTarget<'_>,
        block: &Block,
        id: BlockId,
    ) -> Result<DisconnectResult, Fatal> {
        NullConnector.disconnect_block(cx, v, target, block, id)
    }

    fn test_block_validity(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        checks: &dyn ProposalChecks,
        block: &Block,
        prev: BlockId,
        pow: bool,
        merkle: bool,
    ) -> Result<ConnectOutcome, Fatal> {
        NullConnector.test_block_validity(cx, v, checks, block, prev, pow, merkle)
    }
}

/// D47 item 10 (b3's latch): V never answers a `FlushDue` between a connector's commit and its
/// tip step. One raised during a1's connect is taken by a1's decision and frozen at the step's
/// pair, never at the parent the commit ran on; V's next wait finds none pending.
#[test]
fn a_flush_due_raised_at_a_connectors_commit_is_answered_after_the_tip_step() {
    let w = ModelW::default();
    let armed = Arc::new(AtomicBool::new(false));
    let rig = Rig::with(RigConfig {
        flush: Some(w.site()),
        connector: Some(Box::new(DueOnCommit {
            w: w.clone(),
            armed: armed.clone(),
        })),
        ..RigConfig::default()
    });
    armed.store(true, Ordering::SeqCst);
    let a1 = &branch(&rig.p, &rig.genesis, 1, 0)[0];
    rig.accept(a1);
    let step = w
        .sites()
        .into_iter()
        .find(|s| s.mode == FlushStateMode::IfNeeded && s.after.is_some_and(|(t, _)| t.height == 1))
        .and_then(|s| s.after)
        .expect("a1's step");
    assert_eq!(w.await_due_freezes(1), [step]);
    assert!(w.lock().dues.iter().all(|&h| h == Some(1)), "{:?}", w.lock().dues);
    rig.assert_no_abort();
}

/// D47 item 6 under item 10: a `FlushDue` V answers before init's ABC connects genesis finds no
/// tip and makes no freeze; it stays pending until a decision with a tip, at genesis, takes it.
#[test]
fn a_flush_due_before_genesis_is_connected_waits_for_a_tip() {
    let w = ModelW::default();
    let wm = w.clone();
    let (sent, seen) = mpsc::channel();
    let rig = Rig::with(RigConfig {
        flush: Some(w.site()),
        // V is idle here: it answers at its select, before init's ABC.
        before_init: Some(Box::new(move |_: &ChainHandle| {
            wm.flush_due();
            let m = wm.wait_for(|m| !m.dues.is_empty());
            sent.send((m.dues.clone(), m.due)).expect("the test waits");
        })),
        ..RigConfig::default()
    });
    assert_eq!(seen.recv(), Ok((vec![None], true)));
    let at = last_pair(&w);
    assert_eq!(at.0, block_ref(&rig, &rig.genesis));
    assert_eq!(w.await_due_freezes(1), [at]);
    rig.assert_no_abort();
}

/// D47 item 10: V's stop services W (D45) and answers no `FlushDue`; Core's shutdown flush is
/// `FlushStateToDisk(ALWAYS)`, not a subscriber's.
#[test]
fn v_answers_no_flush_due_at_its_stop() {
    let w = ModelW::default();
    let mut rig = Rig::with(RigConfig {
        flush: Some(w.site()),
        ..RigConfig::default()
    });
    // Raised without a ring: nothing wakes V for it before the stop.
    w.lock().due = true;
    assert!(rig.stop().fatal.is_none());
    let m = w.lock();
    assert_eq!((m.services, m.stops), (1, 1));
    assert!(m.due && m.dues.is_empty());
    drop(m);
    rig.assert_no_abort();
}

/// D60 (Opus N7b-2 r1 m-1): a `FlushDue` a decision took while a job was in flight is kept for
/// the release (CH2b b3 D24), and V's drain park is untimed, so only W's ring on `Released`
/// lets V's next `answer_due` freeze it, at the stepped pair the hold still waits at.
#[test]
fn a_flush_due_kept_for_the_job_in_flight_is_answered_after_its_release() {
    let w = ModelW::default();
    let rig = Rig::with(RigConfig {
        feed_capacity: Some(1),
        flush: Some(w.site()),
        ..RigConfig::default()
    });
    let main = branch(&rig.p, &rig.genesis, 2, 0);
    let side = branch(&rig.p, &rig.genesis, 3, 1);
    for m in main.iter().chain(&side[..2]) {
        rig.accept(m);
    }
    w.lock().in_flight = true;
    // Idle V answers with the job in flight: nothing freezes, and the next decision takes it.
    w.flush_due();
    drop(w.wait_for(|m| m.dues.len() == 1));
    let before = w.sites().len();
    let (stepped, go, done) = gated_subscriber(&rig, 5);
    let w_thread = w_in_hold(&w, &rig, stepped, go, rig.id(&main[1]), |w| {
        // V's answers in step 2's drain, still in flight, freeze nothing. The idle ring left a
        // permit on V's parker, which only the drain takes: its first park returns at once, so
        // V parks for good after its second answer, with no permit left (Validator r1 #1).
        let m = w.wait_for(|m| m.dues.len() >= 3);
        assert!(m.due_freezes.is_empty(), "kept for the release");
        drop(m);
        w.release();
        w.await_due_freezes(1)
    });
    rig.accept(&side[2]);
    let freezes = w_thread();
    assert_eq!(done().len(), 5);
    let sites = w.sites().split_off(before);
    let second = sites[2].after.expect("the second step's pair");
    assert_eq!(second.0, block_ref(&rig, &rig.genesis));
    assert_eq!(freezes, [second]);
    rig.assert_no_abort();
}

/// D61 (Opus N7b-2 r1 m-2): each DisconnectTip moves the prune locks back at Core's place
/// (`validation.cpp:3292-3301`), with the disconnected block's height, after its commit and
/// before its IF_NEEDED site; a connect does not.
#[test]
fn a_disconnect_moves_the_prune_locks_back_before_its_flush_site() {
    let (w, commits) = (ModelW::default(), Arc::new(AtomicUsize::new(0)));
    w.lock().commits = Some(commits.clone());
    let rig = Rig::with(RigConfig {
        flush: Some(w.site()),
        connector: Some(Box::new(DisconnectCommits(commits, NullConnector))),
        ..RigConfig::default()
    });
    let main = branch(&rig.p, &rig.genesis, 2, 0);
    let side = branch(&rig.p, &rig.genesis, 3, 1);
    for m in main.iter().chain(&side[..2]) {
        rig.accept(m);
    }
    assert!(w.lock().movebacks.is_empty(), "connects move no lock");
    rig.accept(&side[2]);
    let (movebacks, sites) = {
        let m = w.lock();
        (m.movebacks.clone(), m.sites.clone())
    };
    assert_eq!(movebacks.iter().map(|&(h, ..)| h).collect::<Vec<_>>(), [2, 1]);
    for (k, (&(h, n, c), m)) in movebacks.iter().zip([&main[1], &main[0]]).enumerate() {
        assert_eq!(c, k + 1, "after this disconnect's DisconnectBlock committed");
        let site = sites[n];
        assert_eq!(
            (site.mode, site.at.map(|(t, _)| t)),
            (FlushStateMode::IfNeeded, Some(block_ref(&rig, m)))
        );
        assert_eq!(h, m.height);
    }
    rig.assert_no_abort();
}

/// D61 (Opus N7b-2 r2 n-1): a `DisconnectBlock` that fails moves no lock back, as Core's
/// `DisconnectTip` returns at `validation.cpp:3281-3284`, before `:3292`. In a two-block reorg
/// whose second disconnect is refused, the locks move back once, for the first, and ABC's step
/// aborts (`:3621`).
#[test]
fn a_failed_disconnect_moves_no_prune_lock_back() {
    const DISCONNECT_FAILED: &str = "Failed to disconnect block; see debug.log for details";
    let w = ModelW::default();
    let commits = Arc::new(AtomicUsize::new(0));
    let refused = Arc::new(OnceLock::new());
    w.lock().commits = Some(commits.clone());
    let refuses = RefusesDisconnect(refused.clone());
    let mut rig = Rig::with(RigConfig {
        flush: Some(w.site()),
        connector: Some(Box::new(DisconnectCommits(commits, refuses))),
        ..RigConfig::default()
    });
    let main = branch(&rig.p, &rig.genesis, 2, 0);
    let side = branch(&rig.p, &rig.genesis, 3, 1);
    for m in main.iter().chain(&side[..2]) {
        rig.accept(m);
    }
    refused.set(main[0].hash()).expect("once");
    let reply = rig.v.process_block(side[2].block.clone(), true, None, true);
    assert_eq!(reply.recv_timeout(SOON).map(|r| r.map(|_| ())), Some(Err(WaitError::Disconnected)));
    assert_eq!(rig.stop().fatal.map(|f| f.0).as_deref(), Some(DISCONNECT_FAILED));
    assert_eq!(rig.abort.messages(), [DISCONNECT_FAILED]);
    let movebacks = w.lock().movebacks.clone();
    assert!(matches!(movebacks.as_slice(), [(2, _, 1)]), "{movebacks:?}");
}

/// The connector `C`, counting each `DisconnectBlock` that committed.
struct DisconnectCommits<C>(Arc<AtomicUsize>, C);

impl<C: BlockConnector> BlockConnector for DisconnectCommits<C> {
    fn connect_block(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        target: ViewTarget<'_>,
        block: &Block,
        checked: bool,
        id: BlockId,
        st: &mut BlockStatusWriter<'_>,
    ) -> Result<ConnectOutcome, Fatal> {
        self.1.connect_block(cx, v, target, block, checked, id, st)
    }

    fn disconnect_block(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        target: ViewTarget<'_>,
        block: &Block,
        id: BlockId,
    ) -> Result<DisconnectResult, Fatal> {
        let result = self.1.disconnect_block(cx, v, target, block, id)?;
        // Core's DisconnectTip commits only on DISCONNECT_OK.
        if matches!(result, DisconnectResult::Ok) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
        Ok(result)
    }

    fn test_block_validity(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        checks: &dyn ProposalChecks,
        block: &Block,
        prev: BlockId,
        pow: bool,
        merkle: bool,
    ) -> Result<ConnectOutcome, Fatal> {
        self.1.test_block_validity(cx, v, checks, block, prev, pow, merkle)
    }
}
