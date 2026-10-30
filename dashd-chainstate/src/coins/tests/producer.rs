//! [`PrefetchProducer`] (CH4-1): V's credit bounds the requests in flight, a worker's read error
//! and a worker's panic reach V through `drain`, the drop cancels what is queued, and a step-locked
//! worker interleaved every way with V's and W's steps around a release never brings back a coin F
//! changed (CH2 §2.4.3; DEC-142 O5). The interleavings also catch the fence's planted bugs X10 and
//! X35.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use dash_consensus::coins::Coin;
use dashd_store::ErrorKind;

use super::*;
use crate::coins::producer::Stage;
use crate::coins::{CoinSrc, PrefetchProducer};
use crate::store::{AbortMark, CoinsReadError};

/// A worker's parking place: it reports each stage it reaches, then waits for a permit. Closing
/// it lets every wait through, so a failed test never leaves a worker parked.
#[derive(Default)]
struct StepLock {
    state: Mutex<Lock>,
    cv: Condvar,
}

#[derive(Default)]
struct Lock {
    reached: Option<Stage>,
    permits: u32,
    closed: bool,
}

impl StepLock {
    /// The state, whoever panicked holding it: a failed test still closes the lock.
    fn lock(&self) -> MutexGuard<'_, Lock> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The worker's side, after it reaches `stage`.
    fn arrive(&self, stage: Stage) {
        let mut s = self.lock();
        s.reached = Some(stage);
        self.cv.notify_all();
        let wait = self.cv.wait_while(s, |s| s.permits == 0 && !s.closed);
        let mut s = wait.unwrap_or_else(PoisonError::into_inner);
        s.permits = s.permits.saturating_sub(1);
    }

    /// The stage the worker reached next.
    fn reached(&self) -> Stage {
        let wait = self
            .cv
            .wait_timeout_while(self.lock(), Duration::from_secs(60), |s| s.reached.is_none());
        let reached = wait.unwrap_or_else(PoisonError::into_inner).0.reached.take();
        reached.expect("the worker never reached its next stage")
    }

    /// Lets the worker run to its next stage, and waits for it there.
    fn step(&self) -> Stage {
        self.lock().permits += 1;
        self.cv.notify_all();
        self.reached()
    }

    fn close(&self) {
        self.lock().closed = true;
        self.cv.notify_all();
    }
}

/// Closes the lock as it drops: declared after the producer, it drops first, so the producer's
/// drop never joins a parked worker.
struct Opened(Arc<StepLock>);

impl Drop for Opened {
    fn drop(&mut self) {
        self.0.close();
    }
}

/// A producer of one worker over `w`'s database, parked at each stage on the returned lock.
fn gated(w: &World, depth: usize) -> (PrefetchProducer, Opened) {
    let lock = Arc::new(StepLock::default());
    let gate = {
        let lock = lock.clone();
        Arc::new(move |s| lock.arrive(s))
    };
    (PrefetchProducer::gated(w.db.clone(), 1, depth, gate).unwrap(), Opened(lock))
}

/// V keeps at most `depth` requests in flight: a request past it is refused, and a drain returns
/// the credit. Each batch V drains is admitted behind the fence.
#[test]
fn the_credit_bounds_the_requests_in_flight() {
    let mut w = World::with_dbcache(16);
    let mut r = Rng::new(51);
    let ks: Vec<_> = (0..3).map(|i| random_key(&mut r, i)).collect();
    for (i, k) in ks.iter().enumerate() {
        w.db_set(k, Some(&p2pkh(i as u64)));
    }
    let (mut producer, lock) = gated(&w, 2);
    assert!(producer.request(w.request(vec![ks[0]])));
    assert!(producer.request(w.request(vec![ks[1]])));
    // The worker holds the first, so the channel has room: only the credit refuses the third.
    assert_eq!(lock.0.reached(), Stage::Took);
    assert!(!producer.request(w.request(vec![ks[2]])), "depth 2 is in flight");
    assert_eq!(producer.in_flight(), 2);
    // The worker reads both batches and sends them.
    for want in [Stage::Snapshot, Stage::Read, Stage::Sent, Stage::Took] {
        assert_eq!(lock.0.step(), want);
    }
    for want in [Stage::Snapshot, Stage::Read, Stage::Sent] {
        assert_eq!(lock.0.step(), want);
    }
    assert_eq!(producer.drain(&mut w.cache, 0).unwrap(), 2);
    assert_eq!(producer.in_flight(), 0);
    assert!(producer.request(w.request(vec![ks[2]])), "the drain returned the credit");
    let before = w.db_reads();
    assert!(w.cache.fetch(&ks[0]).unwrap().is_some() && w.cache.fetch(&ks[1]).unwrap().is_some());
    assert_eq!(w.db_reads(), before, "both came from the map");
}

/// Drains `producer` until it returns an error, polling for at most 60 s.
fn drain_until_err(producer: &mut PrefetchProducer, w: &mut World) -> CoinsReadError {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match producer.drain(&mut w.cache, 0) {
            Err(e) => return e,
            Ok(n) => assert_eq!(n, 0),
        }
        assert!(Instant::now() < deadline, "the worker's read error never reached drain");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// A read error (CH2 §2.4.4 rule 4): the worker's snapshot read trips the latch and tells the abort
/// sink at the read, hands the error back and stops; `drain` returns it. The requests queued behind
/// it are never read: the next drain finds the worker gone, joins it and returns their credit, and
/// the producer refuses requests from then on.
#[test]
fn a_workers_read_error_reaches_drain() {
    let mut w = World::with_dbcache(16);
    let mut r = Rng::new(52);
    let ks: Vec<_> = (0..3).map(|i| random_key(&mut r, i)).collect();
    w.db_set(&ks[0], Some(&p2pkh(1)));
    w.db.inner().fail_reads(Some(ErrorKind::Io));
    let mut producer = PrefetchProducer::spawn(w.db.clone(), 1, 4).unwrap();
    for k in &ks {
        assert!(producer.request(w.request(vec![*k])));
    }
    let err = drain_until_err(&mut producer, &mut w);
    assert_eq!(err.kind(), ErrorKind::Io);
    assert!(w.db.latch().tripped().is_some());
    assert!(matches!(&w.sink.marks()[..], [AbortMark::ReadFailed(_)]));
    assert_eq!(producer.drain(&mut w.cache, 0).unwrap(), 0, "the worker has stopped");
    assert_eq!(producer.in_flight(), 0);
    assert!(!producer.request(w.request(vec![ks[0]])), "a stopped producer refuses requests");
}

/// A worker's panic reaches V: once every worker has stopped, `drain` joins them and raises it.
#[test]
fn a_workers_panic_reaches_drain() {
    let mut w = World::with_dbcache(16);
    let k = random_key(&mut Rng::new(54), 0);
    let gate: crate::coins::producer::Gate = Arc::new(|s| {
        assert_ne!(s, Stage::Read, "a planted worker panic");
    });
    let mut producer = PrefetchProducer::gated(w.db.clone(), 1, 2, gate).unwrap();
    assert!(producer.request(w.request(vec![k])));
    let deadline = Instant::now() + Duration::from_secs(60);
    let panic = loop {
        let drained = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            producer.drain(&mut w.cache, 0)
        }));
        match drained {
            Err(panic) => break panic,
            Ok(n) => assert_eq!(n.unwrap(), 0),
        }
        assert!(Instant::now() < deadline, "the worker's panic never reached drain");
        std::thread::sleep(Duration::from_millis(1));
    };
    let msg = panic.downcast_ref::<String>().map(String::as_str).unwrap_or_default();
    assert!(msg.contains("a planted worker panic"), "{msg}");
    assert_eq!(producer.in_flight(), 0);
    assert!(!producer.request(w.request(vec![k])), "a stopped producer refuses requests");
}

/// The drop cancels the queued requests: the worker finishes the batch it took, and no other.
#[test]
fn the_drop_cancels_the_queued_requests() {
    let w = World::with_dbcache(16);
    let mut r = Rng::new(55);
    let (mut producer, lock) = gated(&w, 3);
    for i in 0..3 {
        assert!(producer.request(w.request(vec![random_key(&mut r, i)])));
    }
    assert_eq!(lock.0.reached(), Stage::Took);
    let queue = producer.queue();
    assert_eq!(queue.len(), 2);
    std::thread::scope(|s| {
        // A failure here unwinds before the scope joins the dropper, which waits on the worker.
        let _open = Opened(lock.0.clone());
        let dropper = s.spawn(move || drop(producer));
        let deadline = Instant::now() + Duration::from_secs(60);
        while !queue.is_empty() {
            assert!(Instant::now() < deadline, "the drop never emptied the queue");
            std::thread::sleep(Duration::from_millis(1));
        }
        for want in [Stage::Snapshot, Stage::Read, Stage::Sent] {
            assert_eq!(lock.0.step(), want);
        }
        lock.0.close();
        dropper.join().unwrap();
    });
    assert_eq!(lock.0.lock().reached, None, "the worker took no other request");
}

/// V's and W's steps around one flush. Keys: `a` and `b` are in the database; V spends `a` and adds
/// `c` before the freeze, W writes F, V releases, then spends `b`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Dispatch,
    SpendA,
    AddC,
    Freeze,
    Write,
    Release,
    SpendB,
    Drain,
    /// The worker's next stage.
    Worker,
}

const FLUSH: [Step; 6] =
    [Step::SpendA, Step::AddC, Step::Freeze, Step::Write, Step::Release, Step::SpendB];

/// Every program: the dispatch at each point of [`FLUSH`], an extra drain at each point after it
/// (or none), and the worker's three stages after the dispatch at every interleaving. A drain ends
/// each program.
fn programs() -> Vec<Vec<Step>> {
    let mut out = Vec::new();
    for at in 0..=FLUSH.len() {
        let after = &FLUSH[at..];
        for drain in (0..=after.len()).map(Some).chain([None]) {
            let mut v: Vec<Step> = after.to_vec();
            if let Some(d) = drain {
                v.insert(d, Step::Drain);
            }
            // The worker's 3 stages among v's steps: every choice of 3 of the n + 3 places.
            let n = v.len() + 3;
            for mask in 0u32..1 << n {
                if mask.count_ones() != 3 {
                    continue;
                }
                let mut rest = v.iter();
                let mut p: Vec<Step> = FLUSH[..at].to_vec();
                p.push(Step::Dispatch);
                p.extend((0..n).map(|i| {
                    if mask & 1 << i != 0 {
                        Step::Worker
                    } else {
                        *rest.next().unwrap()
                    }
                }));
                p.push(Step::Drain);
                out.push(p);
            }
        }
    }
    out
}

/// What a program left: each key's coin through the cache, or the read failure that stopped it.
type Outcome = Result<[Option<Coin>; 3], ErrorKind>;

/// The outcome every program must leave: `a` and `b` spent, `c` unspent.
fn want() -> [Option<Coin>; 3] {
    [None, None, Some(p2pkh(3).as_ref().to_coin())]
}

/// Runs `program` on a new world that `setup` prepares (a planted bug).
fn run(program: &[Step], xcheck: bool, setup: &dyn Fn(&mut World)) -> Outcome {
    let mut w = World::with_dbcache(16);
    w.cache.set_xcheck(xcheck);
    setup(&mut w);
    let mut r = Rng::new(53);
    let [a, b, c] = [0, 1, 2].map(|i| random_key(&mut r, i));
    w.db_set(&a, Some(&p2pkh(1)));
    w.db_set(&b, Some(&p2pkh(2)));
    let (mut producer, lock) = gated(&w, 1);
    let mut f = None;
    let mut stages = [Stage::Snapshot, Stage::Read, Stage::Sent].into_iter();
    let kind = |e: CoinsReadError| e.kind();
    for &step in program {
        match step {
            Step::Dispatch => {
                assert!(producer.request(w.request(vec![a, b, c])));
                assert_eq!(lock.0.reached(), Stage::Took);
            }
            Step::SpendA => assert!(w.cache.spend(&a).map_err(kind)?),
            Step::AddC => w.cache.add(&c, CoinSrc::Bytes(p2pkh(3).as_ref()), false).unwrap(),
            Step::Freeze => f = Some(w.cache.freeze_at(1, 0)),
            Step::Write => write_layer(&w.db, f.as_ref().unwrap().coins()),
            Step::Release => {
                let f = f.take().unwrap();
                let generation = f.generation();
                drop(f);
                w.cache.release(generation);
            }
            Step::SpendB => assert!(w.cache.spend(&b).map_err(kind)?),
            Step::Drain => {
                producer.drain(&mut w.cache, 0).map_err(kind)?;
            }
            Step::Worker => assert_eq!(Some(lock.0.step()), stages.next()),
        }
    }
    assert_eq!(producer.in_flight(), 0, "{program:?}");
    let mut coin = |k| w.cache.fetch(k).map(|c| c.map(|c| c.to_coin())).map_err(kind);
    Ok([coin(&a)?, coin(&b)?, coin(&c)?])
}

/// CH2 §2.4.3's interleaving test with a real worker thread: every order of the worker's snapshot,
/// read and send against V's spend, add, freeze, release and drains and W's write, with the
/// dispatch at every point of the flush. Each leaves `a` and `b` spent and `c` unspent, through
/// the cache, and with xcheck on no read differs from the database's.
#[test]
fn a_parked_worker_never_brings_back_what_a_flush_changed() {
    let programs = programs();
    assert!(programs.len() > 1_000, "{}", programs.len());
    for program in &programs {
        for xcheck in [false, true] {
            assert_eq!(run(program, xcheck, &|_| ()), Ok(want()), "xcheck {xcheck}: {program:?}");
        }
    }
}

/// The interleavings catch the fence's planted bugs X10 (a stale batch admitted) and X35 (a
/// release keeps the map): each brings back `a` with xcheck off and fails xcheck's read with it on.
#[cfg(feature = "planted-bugs")]
#[test]
fn the_interleavings_catch_x10_and_x35() {
    use crate::coins::Planted;

    let programs = programs();
    let mut resurrected = want();
    resurrected[0] = Some(p2pkh(1).as_ref().to_coin());
    for bug in [Planted::X10, Planted::X35] {
        let (mut back, mut refused) = (0, 0);
        for program in &programs {
            for xcheck in [false, true] {
                match run(program, xcheck, &|w| w.cache.plant(bug)) {
                    Ok(got) if got == resurrected && !xcheck => back += 1,
                    Err(ErrorKind::Corruption) if xcheck => refused += 1,
                    got => assert_eq!(got, Ok(want()), "{bug:?}, xcheck {xcheck}: {program:?}"),
                }
            }
        }
        assert!(back > 0 && refused > 0, "{bug:?}: {back} back, {refused} refused");
    }
}
