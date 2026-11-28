//! The header step (PLAN-r1 §2.2): commit before publish, the n8 skip, the closing store and
//! the writer's wake on every generation.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use dash_chain::{
    ChainEvent, ChainLockRef, ChainReadSnapshot, ClOutcome, FeedSource, Handoff, Needs, Next,
    Parker, PeerId, Status, SubscriberFeed, TipStep,
};
use dash_clock::MockClock;
use dash_clock::NodeSeconds;
use dash_num::{Arith256, CompactTarget};
use dash_primitives::{BlockHeader, MerkleRoot};
use dash_types::Numeric;

use super::*;
use crate::bridge::headers::{header_from_base, header_from_consensus, BridgedHeaders};
use crate::bridge::BridgeError;
use crate::client::chain::step::{self, StepInput};

/// Each header comes more than four spacings after its parent, so devnet's
/// minimum-difficulty rule (`pow.cpp:197`) asks powLimit's bits.
pub(super) const SPACING: u32 = 601;
pub(crate) const POW_LIMIT_BITS: u32 = 0x207f_ffff;
pub(crate) const PEER: PeerId = PeerId(7);
const NOW: NodeSeconds = NodeSeconds(2_000_000);

/// `n` devnet headers on `prev`, the first `SPACING` s after `time`; `salt` tells branches apart.
pub(crate) fn mine(prev: BlockHash, time: u32, n: u32, salt: u8) -> Vec<Header> {
    mine_bits(prev, time, n, salt, POW_LIMIT_BITS)
}

/// [`mine`] with `bits`, at most powLimit: each hash meets `bits`' target.
pub(crate) fn mine_bits(prev: BlockHash, time: u32, n: u32, salt: u8, bits: u32) -> Vec<Header> {
    let mut prev = prev;
    let target = CompactTarget::new(bits).expand().value;
    (1..=n)
        .map(|i| {
            let h = (0..)
                .map(|nonce| {
                    let t = time + SPACING * i;
                    let root = MerkleRoot::from([salt; 32]);
                    Header::new(BlockHeader::new(0x2000_0000, prev, root, t, bits, nonce))
                })
                .find(|h| Arith256::from_lendian(*h.hash().as_bytes()) <= target)
                .expect("a nonce");
            prev = h.hash();
            h
        })
        .collect()
}

/// `headers` as the bridge makes them from dashcore's.
pub(crate) fn bridged(headers: &[Header]) -> BridgedHeaders {
    let core: Vec<_> = headers.iter().map(|h| header_from_base(h.raw())).collect();
    BridgedHeaders::from_core(&core)
}

pub(super) fn batch(headers: &[Header]) -> StepInput {
    let [batch] = <[_; 1]>::try_from(bridged(headers).batches(usize::MAX)).unwrap();
    StepInput::Headers {
        peer: PEER,
        batch,
        lead: None,
    }
}

fn anchor() -> AnchorPack {
    pack(20000, 1_000_000)
}

#[test]
fn a_header_step_commits_before_it_publishes_and_reopens_at_its_tip() {
    let dir = tempfile::tempdir().unwrap();
    let h = mem(&dir);
    let s = setup(false, None);
    let (mut chain, _) = open_chain(&h, &s).unwrap();
    let headers = mine(anchor().hash, 1_000_000, 10, 1);
    let epoch = chain.snapshot().epoch;

    let committed = step::commit(&mut chain, &h, batch(&headers)).unwrap();
    assert_eq!(h.view().active.hi, Some(20010), "stage 3 committed the tip");
    assert_eq!(chain.snapshot().epoch, epoch, "nothing is published before stage 4");
    let out = step::publish(&mut chain, committed, &mut step::StepWait::bare(&Parker::new()), NOW)
        .unwrap();
    assert!(out.stop.is_none(), "{:?}", out.stop);
    let last = headers[9].hash();
    assert_eq!((tip(&chain), chain.snapshot().tip.tip_hash), (last, last));
    drop(chain);

    let (chain, _) = open_chain(&h, &s).unwrap();
    assert_eq!(tip(&chain), last);
    assert!(!s.fail.is_tripped());
}

#[test]
fn a_quiet_tick_commits_and_publishes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let h = mem(&dir);
    let s = setup(false, None);
    let (mut chain, _) = open_chain(&h, &s).unwrap();
    let parker = Parker::new();
    step::step_parked(&mut chain, &h, batch(&mine(anchor().hash, 1_000_000, 3, 1)), &parker, NOW)
        .unwrap();
    assert!(!chain.cl_enabled_observed());
    step::step_parked(&mut chain, &h, StepInput::Tick, &parker, NOW).unwrap();
    assert!(chain.cl_enabled_observed(), "the first tick observes SPORK_19 on");
    let (stored, _) = open_chain(&h, &s).unwrap();
    assert!(stored.cl_enabled_observed(), "and commits it");
    drop(stored);
    let wrote = Arc::new(AtomicBool::new(false));
    {
        let wrote = Arc::clone(&wrote);
        h.interleave(Point::WriterWaiting, move |_| wrote.store(true, Ordering::SeqCst));
    }
    let epoch = chain.snapshot().epoch;
    step::step_parked(&mut chain, &h, StepInput::Tick, &parker, NOW).unwrap();
    assert!(!wrote.load(Ordering::SeqCst), "the tick took the writer");
    assert_eq!(chain.snapshot().epoch, epoch);
}

#[tokio::test]
async fn a_step_while_the_store_closes_stops_the_task_and_trips_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = SpvStore::open(dir.path()).unwrap();
    let s = setup(false, None);
    let host = SpvChainHost::open(store.clone(), s.clone()).await.unwrap();
    let mut source = host.source();
    let closed = store.close();
    let out = host.handle().headers(PEER, bridged(&mine(anchor().hash, 1_000_000, 1, 1))).await;
    assert!(matches!(out, Err(SpvError::Storage(StorageError::ShuttingDown))), "{out:?}");
    source.wait_for(|now| matches!(now.state, SourceState::Stopped)).await.unwrap();
    assert!(!s.fail.is_tripped());
    drop(closed);
}

/// A runtime shutting down drops the store jobs it never started; each is refused as a stop
/// refuses it, which the task stops on without tripping anything.
#[test]
fn a_job_the_runtime_drops_at_shutdown_is_refused_as_a_stop() {
    let (mem_dir, spv_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (h, store) = (mem(&mem_dir), SpvStore::open(spv_dir.path()).unwrap());
    let rt = tokio::runtime::Runtime::new().unwrap();
    let handle = rt.handle().clone();
    rt.shutdown_timeout(Duration::ZERO);
    let (on_mem, on_spv) = handle.block_on(async { (h.job(|| ()).await, store.job(|| ()).await) });
    for out in [on_mem, on_spv] {
        assert!(matches!(out, Err(SpvError::Storage(StorageError::ShuttingDown))), "{out:?}");
    }
}

/// A subscriber at the published tip of `source`, drained on its own thread from `after` on
/// until `done`.
/// A subscriber live at `source`'s published tip.
fn subscribe(source: &ChainSource) -> Box<dyn SubscriberFeed> {
    let SourceState::Live {
        feeds: LiveFeeds(feeds),
        ..
    } = &source.state
    else {
        panic!("not live");
    };
    let id = feeds.register(Needs::default()).unwrap();
    let at = FeedSource::<()>::snapshot(feeds).tip();
    let Ok(Handoff::Live {
        feed,
        ..
    }) = feeds.subscribe_at(id, &at, &mut || Ok::<(), ()>(()))
    else {
        panic!("subscribe_at");
    };
    feed
}

/// Every event `feed` holds now.
fn drain(feed: &mut dyn SubscriberFeed) -> Vec<ChainEvent> {
    let mut events = Vec::new();
    loop {
        match feed.next() {
            Next::Event(_) => events.extend(feed.pop()),
            Next::Control(_) => {}
            Next::Empty => return events,
        }
    }
}

fn slow_subscriber(source: &ChainSource, after: Duration, done: Arc<AtomicBool>) {
    let mut feed = subscribe(source);
    std::thread::spawn(move || {
        while matches!(feed.next(), Next::Empty | Next::Control(_)) {
            if done.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        std::thread::sleep(after);
        while !done.load(Ordering::SeqCst) {
            match feed.next() {
                Next::Event(_) => drop(feed.pop()),
                Next::Control(_) => {}
                Next::Empty => std::thread::sleep(Duration::from_millis(1)),
            }
        }
    });
}

/// More events than a subscriber holds, on the opened chain and on a replacement: the writer
/// parks once the subscriber's queue is full, and only the subscriber making space wakes it.
/// A writer that never wakes fails the test; its own runtime leaves the parked job behind.
#[test]
fn the_writer_wakes_on_subscriber_space_on_every_generation() {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let h = mem(&dir);
    let s = setup(true, None);
    let woke = rt.block_on(async {
        let host = SpvChainHost::open_on(h.clone(), s.clone()).await.unwrap();
        for (gen, at) in [(ChainGen(1), anchor()), (ChainGen(2), pack(10000, 1_000_000))] {
            if gen == ChainGen(2) {
                let out = host.handle().replace(pack_anchor(&at)).await;
                assert!(matches!(out, ReplaceOutcome::Replaced(ChainGen(2))), "{out:?}");
            }
            let done = Arc::new(AtomicBool::new(false));
            slow_subscriber(&host.source().borrow(), Duration::from_millis(500), Arc::clone(&done));
            let headers = mine(at.hash, 1_000_000, 1100, 2);
            let handle = host.handle();
            let reply = tokio::time::timeout(
                Duration::from_secs(60),
                handle.headers(PEER, bridged(&headers)),
            )
            .await;
            done.store(true, Ordering::SeqCst);
            let Ok(reply) = reply else {
                return Err(gen);
            };
            let reply = reply.unwrap();
            assert_eq!((reply.gen, reply.stop), (gen, None));
            assert_eq!(reply.snap.tip.tip_hash, headers[1099].hash());
        }
        Ok(())
    });
    rt.shutdown_timeout(Duration::from_secs(1));
    woke.unwrap_or_else(|gen| panic!("{gen:?}: the writer never woke"));
    assert!(!s.fail.is_tripped());
}

#[test]
fn the_bridge_splits_a_batch_into_steps() {
    let headers = mine(anchor().hash, 1_000_000, 5, 1);
    let sizes =
        |b: BridgedHeaders| b.batches(2).iter().map(|b| b.headers.len()).collect::<Vec<_>>();
    assert_eq!(sizes(bridged(&headers)), [2, 2, 1]);
    assert!(sizes(BridgedHeaders::default()).is_empty());
    for b in bridged(&headers).batches(2) {
        let own: Vec<_> = b.headers.iter().map(Header::hash).collect();
        assert_eq!(b.hashes, own);
    }
}

/// SB12a: a pair whose hash is not its header's X11 never crosses; the batch `accept` reads
/// carries the hashes `dash-consensus` computed, and the store holds those.
#[tokio::test]
async fn header_batch_hashes_come_from_the_bridge() {
    let dir = tempfile::tempdir().unwrap();
    let h = mem(&dir);
    let s = setup(false, None);
    let host = SpvChainHost::open_on(h.clone(), s.clone()).await.unwrap();
    let headers = mine(anchor().hash, 1_000_000, 3, 1);
    let mut pairs: Vec<_> = headers.iter().map(header_from_consensus).collect();
    let good = BridgedHeaders::from_hashed(&pairs).unwrap();
    pairs[1] = crate::types::HashedBlockHeader::with_trusted_hash(
        *pairs[1].header(),
        dashcore::BlockHash::from_byte_array([9; 32]),
    );
    assert_eq!(
        BridgedHeaders::from_hashed(&pairs),
        Err(BridgeError::WrongHash {
            kind: "header"
        })
    );

    let reply = host.handle().headers(PEER, good).await.unwrap();
    assert_eq!(reply.stop, None);
    assert_eq!(reply.snap.tip.tip_hash, headers[2].hash());
    for header in &headers {
        let rec = h.by_hash(&header.hash()).unwrap().expect("stored under its X11");
        assert_eq!(rec.header, *header.raw());
    }
    assert!(h.by_hash(&BlockHash::from([9; 32])).unwrap().is_none());
}

/// A devnet host Established at 20000 with ChainLocks `chainlocks`: A1…A6 active, and B3, B4
/// on A2 stored beside them with less work, each branch by its own step.
pub(crate) struct Forked {
    pub(crate) host: SpvChainHost,
    pub(crate) setup: ChainHostSetup,
    pub(crate) store: Mem,
    pub(crate) a: Vec<Header>,
    pub(crate) b: Vec<Header>,
    /// The snapshots A's step and B's step published.
    after_a: Arc<ChainReadSnapshot>,
    after_b: Arc<ChainReadSnapshot>,
    /// What a subscriber live after A's step received from B's.
    b_events: Vec<ChainEvent>,
    _dir: tempfile::TempDir,
}

impl Forked {
    pub(crate) async fn new(chainlocks: Option<bool>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = mem(&dir);
        let mut setup = setup(false, None);
        setup.config.chainlocks_enabled = chainlocks;
        setup.sporks = crate::client::chain_sporks(&setup.config, setup.llmq.params());
        let host = SpvChainHost::open_on(store.clone(), setup.clone()).await.unwrap();
        let a = mine(anchor().hash, 1_000_000, 6, 1);
        let b = mine(a[1].hash(), 1_000_000 + 2 * SPACING, 2, 2);
        let after_a = host.handle().headers(PEER, bridged(&a)).await.unwrap();
        assert_eq!(after_a.stop, None);
        let mut feed = subscribe(&host.source().borrow());
        let after_b = host.handle().headers(PEER, bridged(&b)).await.unwrap();
        assert_eq!(after_b.stop, None);
        let b_events = drain(feed.as_mut());
        Forked {
            host,
            setup,
            store,
            a,
            b,
            after_a: after_a.snap,
            after_b: after_b.snap,
            b_events,
            _dir: dir,
        }
    }

    /// A ChainLock on B4, the lighter branch's tip.
    pub(crate) fn lock_on_b(&self) -> ChainLockRef {
        ChainLockRef {
            height: 20004,
            hash: self.b[1].hash(),
        }
    }

    fn status(&self, h: &Header) -> Status {
        let SourceState::Live {
            reader,
            ..
        } = self.host.source().borrow().state.clone()
        else {
            panic!("not live");
        };
        let id = reader.try_lookup(&h.hash()).unwrap().expect("held");
        reader.try_entry(id).unwrap().expect("held").status
    }
}

/// Rule 4: a branch with less work is stored, and the tip stays on the most work.
#[tokio::test]
async fn less_work_branch_is_stored_not_followed() {
    let f = Forked::new(None).await;
    let disconnected = f.b_events.iter().filter(|e| matches!(e, TipStep::Disconnected { .. }));
    assert_eq!(disconnected.count(), 0, "{:?}", f.b_events);
    assert_eq!(f.after_b.tip.tip_hash, f.a[5].hash(), "B's own step leaves the tip on A6");
    assert_eq!(f.after_b.tip_seq, f.after_a.tip_seq);
    assert_eq!(f.after_b.epoch, f.after_a.epoch + 1, "one status-only publication");
    for h in &f.b {
        assert!(f.store.by_hash(&h.hash()).unwrap().is_some(), "B is stored");
        assert!(!f.status(h).has(Status::CONFLICT_CHAINLOCK));
    }
    assert!(!f.setup.fail.is_tripped());
}

/// Rule 4: a ChainLock on the branch with less work moves the tip to it, and A3, the other
/// branch's root, is marked `BLOCK_CONFLICT_CHAINLOCK`.
#[tokio::test]
async fn chainlocked_branch_wins_over_more_work() {
    let f = Forked::new(None).await;
    let reply = f.host.handle().apply_chainlock(f.lock_on_b()).await.unwrap();
    let ClOutcome::Floor {
        enforced: Some(report),
        ..
    } = &reply.outcome
    else {
        panic!("{:?}", reply.outcome);
    };
    assert_eq!(report.marked.len(), 1, "{report:?}");
    assert_eq!(reply.snap.tip.tip_hash, f.b[1].hash());
    assert_eq!(reply.snap.cl_floor.map(|r| r.hash), Some(f.b[1].hash()));
    assert!(f.status(&f.a[2]).has(Status::CONFLICT_CHAINLOCK));
    assert!(!f.status(&f.a[1]).has(Status::CONFLICT_CHAINLOCK));
    let more = mine(f.a[5].hash(), 1_000_000 + 6 * SPACING, 1, 1);
    let reply = f.host.handle().headers(PEER, bridged(&more)).await.unwrap();
    assert_eq!(reply.snap.tip.tip_hash, f.b[1].hash(), "A stays behind the lock");
    assert!(!f.setup.fail.is_tripped());
}

/// m3: every chain reads the setup's one spork instance, so a value set on it reaches fork
/// choice: with SPORK_19 off the lock is kept but bounds nothing, and the tip stays on A.
#[tokio::test]
async fn the_chain_reads_the_setups_one_spork_instance() {
    let f = Forked::new(None).await;
    f.setup.sporks.set(false);
    let reply = f.host.handle().apply_chainlock(f.lock_on_b()).await.unwrap();
    assert!(matches!(
        reply.outcome,
        ClOutcome::Floor {
            enforced: None,
            ..
        }
    ));
    assert!(!reply.snap.cl_enabled_observed);
    assert_eq!(reply.snap.tip.tip_hash, f.a[5].hash());
}

/// `UpdatedBlockTip` schedules one more check (`chainlock/handler.cpp:190-193`) when a step moves
/// the tip, by headers or by a lock, but not for a stored branch that is not followed, and never
/// in initial block download.
#[test]
fn a_step_that_updates_the_tip_schedules_a_check() {
    for (now, scheduled) in [(1_010_000, true), (2_000_000, false)] {
        let dir = tempfile::tempdir().unwrap();
        let h = mem(&dir);
        let clock = Arc::new(MockClock::at(2_000_000));
        let mut s = setup(false, None);
        s.config.clock = clock.clone();
        let (mut chain, _) = open_chain(&h, &s).unwrap();
        clock.set_mocktime(now);
        let (parker, now) = (Parker::new(), NodeSeconds(now));
        let a = mine(anchor().hash, 1_000_000, 6, 1);
        let b = mine(a[1].hash(), 1_000_000 + 2 * SPACING, 2, 2);
        let out = step::step_parked(&mut chain, &h, self::batch(&a), &parker, now).unwrap();
        assert_eq!(out.schedule, scheduled, "a moves the tip, now {now:?}");
        let out = step::step_parked(&mut chain, &h, self::batch(&b), &parker, now).unwrap();
        assert!(!out.schedule, "b is stored, not followed, now {now:?}");
        let lock = ChainLockRef {
            height: 20004,
            hash: b[1].hash(),
        };
        let out =
            step::step_parked(&mut chain, &h, StepInput::ChainLock(lock), &parker, now).unwrap();
        assert_eq!(tip(&chain), b[1].hash());
        assert_eq!(out.schedule, scheduled, "a lock that moves the tip to b, now {now:?}");
    }
}

#[derive(Clone)]
struct Counted(Mem, Arc<std::sync::atomic::AtomicUsize>);

impl StoreHandle for Counted {
    type Engine = MemoryEngine;
    type Io = <Mem as StoreHandle>::Io;

    fn store(&self) -> &Store<MemoryEngine, Self::Io> {
        self.0.store()
    }

    fn job<T: Send + 'static>(
        &self,
        f: impl FnOnce() -> T + Send + 'static,
    ) -> impl std::future::Future<Output = Result<T, SpvError>> + Send {
        self.1.fetch_add(1, Ordering::SeqCst);
        self.0.job(f)
    }
}

/// The task runs the scheduled check as a step of its own before its next input.
#[tokio::test(start_paused = true)]
async fn the_task_runs_a_scheduled_check_before_its_next_input() {
    for (now, jobs) in [(1_010_000, 2), (2_000_000, 1)] {
        let dir = tempfile::tempdir().unwrap();
        let h = Counted(mem(&dir), Default::default());
        let clock = Arc::new(MockClock::at(2_000_000));
        let mut s = setup(false, None);
        s.config.clock = clock.clone();
        let host = SpvChainHost::open_on(h.clone(), s).await.unwrap();
        clock.set_mocktime(now);
        let handle = host.handle();
        let before = h.1.load(Ordering::SeqCst);
        handle.headers(PEER, bridged(&mine(anchor().hash, 1_000_000, 3, 1))).await.unwrap();
        handle.headers(PEER, BridgedHeaders::from_core(&[])).await.unwrap();
        assert_eq!(h.1.load(Ordering::SeqCst) - before, jobs, "now {now}");
    }
}

/// An empty batch runs no step (`net_processing.cpp:3333-3336`): no store job, so no writer,
/// and the reply is the published snapshot.
#[tokio::test(start_paused = true)]
async fn an_empty_batch_runs_no_step() {
    let dir = tempfile::tempdir().unwrap();
    let h = Counted(mem(&dir), Default::default());
    let host = SpvChainHost::open_on(h.clone(), setup(false, None)).await.unwrap();
    let handle = host.handle();
    let a = handle.headers(PEER, bridged(&mine(anchor().hash, 1_000_000, 3, 1))).await.unwrap();
    let before = h.1.load(Ordering::SeqCst);
    let reply = handle.headers(PEER, BridgedHeaders::default()).await.unwrap();
    assert_eq!(h.1.load(Ordering::SeqCst), before, "no store job");
    assert_eq!((reply.gen, reply.stop, reply.last_accepted), (ChainGen(1), None, None));
    assert_eq!(reply.snap.epoch, a.snap.epoch);
}

/// M1: a batch's steps run back to back, so a replacement queued from another handle while its
/// first step runs waits for the whole batch, which one generation takes, as Core takes one
/// `headers` message in one `ProcessNewBlockHeaders` (`net_processing.cpp:3367`).
#[tokio::test]
async fn a_replacement_waits_for_the_whole_batch() {
    let dir = tempfile::tempdir().unwrap();
    let h = mem(&dir);
    let clock = Arc::new(MockClock::at(2_000_000));
    let mut s = setup(true, None);
    s.config.clock = clock.clone();
    let host = SpvChainHost::open_on(h.clone(), s.clone()).await.unwrap();
    clock.set_mocktime(3_000_000);
    let headers = mine(anchor().hash, 1_000_000, 2001, 1);
    let (other, rt) = (host.handle(), tokio::runtime::Handle::current());
    let replaced = Arc::new(Mutex::new(None));
    {
        let replaced = Arc::clone(&replaced);
        h.interleave(Point::WriterWaiting, move |_| {
            let mut replace =
                Box::pin(async move { other.replace(pack_anchor(&pack(10000, 1_000_000))).await });
            let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
            assert!(
                Future::poll(replace.as_mut(), &mut cx).is_pending(),
                "queued, awaiting its reply"
            );
            *replaced.lock() = Some(rt.spawn(replace));
        });
    }
    let reply = host.handle().headers(PEER, bridged(&headers)).await.unwrap();
    assert_eq!((reply.gen, &reply.stop), (ChainGen(1), &None));
    assert_eq!(reply.snap.tip.tip_hash, headers[2000].hash());
    assert_eq!(reply.last_accepted, Some(reply.snap.tip.tip));
    let replace = replaced.lock().take().expect("queued during the first step");
    let out = replace.await.unwrap();
    assert!(matches!(out, ReplaceOutcome::Replaced(ChainGen(2))), "{out:?}");
    assert!(!s.fail.is_tripped());
}

/// A continuous batch larger than a step: the reply's stop counts from the batch's first
/// header, and `last_accepted` is the last header any of its steps accepted, also when the step
/// that stops accepts none.
#[tokio::test]
async fn a_split_batch_is_reported_whole() {
    for bad in [2000, 2002] {
        let dir = tempfile::tempdir().unwrap();
        let h = mem(&dir);
        let clock = Arc::new(MockClock::at(2_000_000));
        let mut s = setup(false, None);
        s.config.clock = clock.clone();
        let host = SpvChainHost::open_on(h.clone(), s.clone()).await.unwrap();
        clock.set_mocktime(3_000_000);
        let mut headers = mine(anchor().hash, 1_000_000, 2010, 1);
        let time = 1_000_000 + SPACING * bad as u32;
        headers[bad] = mine_bits(headers[bad - 1].hash(), time, 1, 3, POW_LIMIT_BITS - 1)[0];
        let rest = mine(headers[bad].hash(), time + SPACING, (2009 - bad) as u32, 3);
        headers.splice(bad + 1.., rest);
        let reply = host.handle().headers(PEER, bridged(&headers)).await.unwrap();
        let Some(dash_chain::HeaderStop::Invalid {
            index,
            reject,
        }) = &reply.stop
        else {
            panic!("{:?}", reply.stop);
        };
        assert_eq!(*index, bad);
        assert!(format!("{reject:?}").contains("bad-diffbits"), "{reject:?}");
        let last = reply.last_accepted.expect("the batch's accepted headers");
        assert_eq!(h.by_id(last).unwrap().unwrap().hash, headers[bad - 1].hash(), "bad {bad}");
        assert_eq!(reply.snap.tip.tip_hash, headers[bad - 1].hash());
    }
}
