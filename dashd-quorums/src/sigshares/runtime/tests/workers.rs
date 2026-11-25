//! W2-3: the dispatcher pass, the sign task, the thread loops, the pool and the lifecycle.

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::fmt;
use std::sync::atomic::AtomicUsize;
use std::sync::{Barrier, Condvar, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use dash_evo::locks::sign_hash;
use dash_llmq::dkg::bls::{secret_to_public, sign};
use dash_llmq::sigenv::{BatchVerdict, ShareBatch, SigningQuorum};
use dash_llmq::sigshares::ticks::{dispatcher_tick, worker_count};

use super::*;
use crate::signing::SigningInterrupt;
use crate::sigshares::pool::{OnPanic, Pool};
use crate::sigshares::runtime::Work;
use crate::sigshares::worker::Pause;

const MISSING: &str = "CollectPendingSigSharesToVerify: ERROR! Unexpected missing quorum with llmqType=100, quorumHash=";

fn id(i: u8) -> [u8; 32] {
    let mut b = [0x5a; 32];
    b[0] = i;
    b
}

fn sk(member: u16) -> BlsSkValue {
    BlsSkValue::from_canonical(&scalar(10 + member as u8), BlsSchemeKind::Basic).unwrap()
}

/// `quorum(threshold)` with member `i`'s key share `sk(i)`; we are member 0.
fn verifiable(threshold: u16) -> Arc<FakeQuorum> {
    let q = quorum(threshold);
    Arc::new(FakeQuorum {
        llmq_type: T,
        quorum_hash: q.quorum_hash,
        params: q.params,
        members: q.members.clone(),
        valid_members: q.valid_members.clone(),
        has_verification_vector: true,
        quorum_public_key: BlsPkValue::invalid(),
        pk_shares: (0..3).map(|i| (i, secret_to_public(&sk(i)))).collect(),
        sk_share: Some(sk(0)),
    })
}

/// Member `member`'s share for `id`, signed with `sk(key)`.
fn signed(member: u16, key: u16, id: [u8; 32]) -> SigShare {
    let sig = sign(&sk(key), &sign_hash(T, &QH.into(), &id, &MSG), false);
    let bytes = SigShareBytes::from_bytes(sig.to_bytes(BlsSchemeKind::Basic));
    SigShare::new(T, QH.into(), member, id.into(), MSG.into(), bytes)
}

fn unsigned(i: u8) -> SigShare {
    SigShare::new(T, QH.into(), 1, id(i).into(), MSG.into(), SigShareBytes::from_bytes([0; 96]))
}

/// `n` shares from `PEER`, one per session.
fn flood(fx: &Fx, n: u8) {
    let shares: Vec<_> = (0..n).map(unsigned).collect();
    for chunk in shares.chunks(25) {
        fx.rt.on_sig_share(PEER, chunk.to_vec());
    }
    assert_eq!(pending(fx), usize::from(n));
}

/// Passes every share and records, per batch, its size and whether S was free.
struct Spy<'a> {
    rt: &'a Rt,
    batches: RefCell<Vec<(usize, bool)>>,
}

impl<'a> Spy<'a> {
    fn new(rt: &'a Rt) -> Self {
        Self {
            rt,
            batches: RefCell::default(),
        }
    }

    fn sizes(&self) -> Vec<usize> {
        self.batches.borrow().iter().map(|b| b.0).collect()
    }
}

impl ShareVerifier for Spy<'_> {
    fn verify_batch<Q: SigningQuorum, R: RecSigReads>(
        &self,
        batch: &ShareBatch<Q>,
        _: &R,
        _: BlsCtx,
    ) -> Result<BatchVerdict, VerifyError<R::Error>> {
        let pushed = batch.by_source.values().map(Vec::len).sum();
        self.batches.borrow_mut().push((pushed, s_free(self.rt)));
        Ok(BatchVerdict {
            bad_sources: BTreeSet::new(),
            pushed,
        })
    }
}

/// Runs a dispatcher task at once on this thread.
fn run(rt: &Rt, w: Work<FakeQuorum>, verifier: &impl ShareVerifier) {
    match w {
        Work::Sign(w) => rt.sign_task(w),
        Work::Verify(batch, _in_flight) => rt.verify_task(&batch, verifier),
    }
}

fn in_flight(fx: &Fx) -> usize {
    fx.rt.in_flight().load(Ordering::SeqCst)
}

/// Two own signs, 40 good shares from `PEER` and one bad share from peer 8.
fn fed(gone: bool) -> Fx {
    let fx = fixture_with(verifiable(2), 0, Arc::new(FakeOperatorSigner::default()));
    for i in 0..2 {
        fx.rt.async_sign(fx.q.clone(), id(i), MSG);
    }
    let good: Vec<_> = (0..40).map(|i| signed(1, 1, id(i))).collect();
    for chunk in good.chunks(20) {
        fx.rt.on_sig_share(PEER, chunk.to_vec());
    }
    fx.rt.on_sig_share(8, vec![signed(2, 1, id(0))]);
    assert_eq!(pending(&fx), 41);
    fx.rt.h.lookup.gone.store(gone, Ordering::Relaxed);
    fx.log.take();
    fx
}

/// The applied effects. The pass applies each batch's effects at once and the tick only at its
/// end, so the pending count `remove_node` sees is dropped.
fn effects(fx: &Fx) -> Vec<Ev> {
    let effects = fx.log.take().into_iter().filter(|e| !matches!(e, Ev::UnderS(_)));
    effects
        .map(|e| match e {
            Ev::RemoveNode(peer, _) => Ev::RemoveNode(peer, None),
            e => e,
        })
        .collect()
}

#[test]
fn a_dispatcher_pass_equals_the_pure_tick() {
    for gone in [false, true] {
        let inline = fed(gone);
        let missing = inline.rt.dispatch_pass(&ReferenceVerifier);

        let pure = fed(gone);
        let mut rng = pure.rt.h.facts.rng();
        let op = Operator(&*pure.rt.h.operator);
        let pre = pure.rt.prefetch();
        let out = pure.rt.locked(&pre, |m, env| {
            let (bls, now) = (env.bls, env.now.ms);
            dispatcher_tick(m, env, &ReferenceVerifier, env.signer, &op, bls, bls, &mut rng, 0, now)
        });
        let out = out.unwrap();
        pure.rt.apply(out.effects);

        assert_eq!(missing, out.missing_quorum);
        let ran = effects(&inline);
        assert_eq!(ran, effects(&pure));
        assert_eq!(pending(&inline), pending(&pure));
        if gone {
            assert_eq!(missing, Some((T.to_base(), QH.into())));
        } else {
            assert_eq!(out.batches, 2);
            assert!(ran.contains(&Ev::Misbehaving(8, 100)), "{ran:?}");
            assert!(ran.iter().any(|e| matches!(e, Ev::Recovered { .. })), "{ran:?}");
        }
        assert_eq!(aborts(&inline), 0);
    }
}

#[test]
fn a_pass_verifies_at_most_four_batches_of_32_with_s_free() {
    let fx = fixture(3, 0);
    flood(&fx, 200);
    let spy = Spy::new(&fx.rt);
    assert_eq!(fx.rt.dispatch_pass(&spy), None);
    assert_eq!(*spy.batches.borrow(), vec![(32, true); 4]);
    assert_eq!(pending(&fx), 200 - 128);
}

#[test]
fn in_flight_batches_count_across_passes_until_their_tasks_end() {
    let fx = fixture(3, 0);
    flood(&fx, 200);
    let mut held = Vec::new();
    fx.rt.dispatcher_pass(|| false, |w| held.push(w));
    assert_eq!((held.len(), in_flight(&fx), pending(&fx)), (4, 4, 200 - 128));
    fx.rt.dispatcher_pass(|| false, |w| held.push(w));
    assert_eq!((held.len(), pending(&fx)), (4, 200 - 128), "no collect while four are in flight");

    drop(held.pop());
    assert_eq!(in_flight(&fx), 3);
    fx.rt.dispatcher_pass(|| false, |w| held.push(w));
    assert_eq!((held.len(), in_flight(&fx), pending(&fx)), (4, 4, 200 - 160));

    let spy = Spy::new(&fx.rt);
    for w in held.drain(..) {
        run(&fx.rt, w, &spy);
    }
    assert_eq!((spy.sizes(), in_flight(&fx)), (vec![32; 4], 0));
    fx.rt.dispatcher_pass(|| false, |w| held.push(w));
    assert_eq!((held.len(), pending(&fx)), (2, 0));
}

struct Panics;

impl ShareVerifier for Panics {
    fn verify_batch<Q: SigningQuorum, R: RecSigReads>(
        &self,
        _: &ShareBatch<Q>,
        _: &R,
        _: BlsCtx,
    ) -> Result<BatchVerdict, VerifyError<R::Error>> {
        panic!("verifier panics");
    }
}

fn no_panic() -> OnPanic {
    Arc::new(|name: &str| panic!("{name} panicked"))
}

#[test]
fn a_panicking_verify_task_leaves_the_in_flight_count_and_its_worker() {
    let fx = fixture(3, 0);
    flood(&fx, 40);
    let mut held = Vec::new();
    fx.rt.dispatcher_pass(|| false, |w| held.push(w));
    assert_eq!(in_flight(&fx), 2);
    let rt = fx.rt.clone();
    let pool = Pool::start(1, &(Arc::new(move |name: &str| rt.panicked(name)) as OnPanic)).unwrap();
    for w in held {
        let rt = fx.rt.clone();
        pool.queue().push(Box::new(move || run(&rt, w, &Panics)));
    }
    let (done, finished) = mpsc::channel();
    pool.queue().push(Box::new(move || done.send(()).unwrap()));
    finished.recv_timeout(Duration::from_secs(30)).unwrap();
    assert_eq!(in_flight(&fx), 0);
    pool.stop();
    assert_eq!(*fx.aborts.lock().unwrap(), ["sigsh-work.0 panicked"]);
}

/// Holds each batch until a permit is released.
#[derive(Default)]
struct Gate {
    entered: AtomicUsize,
    permits: Mutex<usize>,
    open: Condvar,
}

impl Gate {
    fn release(&self, n: usize) {
        *self.permits.lock().unwrap() += n;
        self.open.notify_all();
    }
}

struct Gated(Arc<Gate>);

impl ShareVerifier for Gated {
    fn verify_batch<Q: SigningQuorum, R: RecSigReads>(
        &self,
        batch: &ShareBatch<Q>,
        _: &R,
        _: BlsCtx,
    ) -> Result<BatchVerdict, VerifyError<R::Error>> {
        self.0.entered.fetch_add(1, Ordering::SeqCst);
        let mut permits = self.0.permits.lock().unwrap();
        while *permits == 0 {
            permits = self.0.open.wait(permits).unwrap();
        }
        *permits -= 1;
        Ok(BatchVerdict {
            bad_sources: BTreeSet::new(),
            pushed: batch.by_source.values().map(Vec::len).sum(),
        })
    }
}

fn eventually(what: &str, f: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !f() {
        assert!(Instant::now() < deadline, "never: {what}");
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn pool_verification_leaves_intake_free_and_holds_four_batches_at_most() {
    const OTHER: PeerId = 9;
    let fx = fixture_connected(quorum(3), 0, Arc::new(FakeOperatorSigner::default()), &[OTHER]);
    flood(&fx, 200);
    let gate = Arc::new(Gate::default());
    let interrupt = SigningInterrupt::new();
    let g = gate.clone();
    let threads = fx.rt.start(interrupt.clone(), move || Gated(g.clone())).unwrap();
    eventually("four batches in flight", || in_flight(&fx) == 4);
    eventually("a batch verifying", || gate.entered.load(Ordering::SeqCst) > 0);

    let (rt, (sent, intake)) = (fx.rt.clone(), mpsc::channel());
    thread::spawn(move || {
        rt.on_ses_ann(OTHER, vec![ann(5)]);
        rt.on_sig_share(OTHER, vec![unsigned(250)]);
        sent.send(()).unwrap();
    });
    intake.recv_timeout(Duration::from_secs(30)).expect("intake waits on a verifying batch");
    eventually("S free while batches verify", || s_free(&fx.rt));

    thread::sleep(Duration::from_millis(50));
    assert_eq!((in_flight(&fx), pending(&fx)), (4, 200 - 128 + 1), "no fifth collect");
    gate.release(1);
    eventually("a fifth collect", || pending(&fx) == 200 - 160 + 1 && in_flight(&fx) == 4);

    gate.release(usize::MAX / 2);
    interrupt.trigger();
    threads.join();
    assert_eq!(in_flight(&fx), 0);
    assert_eq!(aborts(&fx), 0);
}

/// The lines `f` logs on this thread, as `regtest_quorums`'s `logged` captures them, each as
/// "LEVEL target message=…".
pub(super) fn logged<T>(f: impl FnOnce() -> T) -> (T, Vec<String>) {
    struct Lines(Mutex<Vec<String>>);
    impl tracing::Subscriber for Lines {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            struct Message(String);
            impl tracing::field::Visit for Message {
                fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn fmt::Debug) {
                    self.0.push_str(&format!(" {}={v:?}", f.name()));
                }
            }
            let meta = event.metadata();
            let mut m = Message(format!("{} {}", meta.level(), meta.target()));
            event.record(&mut m);
            self.0.lock().unwrap().push(m.0);
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }
    // BL-479: never the only registered dispatcher.
    static KEEP: OnceLock<tracing::Dispatch> = OnceLock::new();
    KEEP.get_or_init(|| tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default()));
    let lines = Arc::new(Lines(Mutex::new(Vec::new())));
    let out = tracing::subscriber::with_default(Arc::clone(&lines), f);
    let lines = std::mem::take(&mut *lines.0.lock().unwrap());
    (out, lines)
}

#[test]
fn a_missing_quorum_logs_once_and_stops_collecting() {
    let fx = fixture(3, 0);
    flood(&fx, 40);
    fx.rt.h.lookup.gone.store(true, Ordering::Relaxed);
    let spy = Spy::new(&fx.rt);
    let (missing, lines) = logged(|| fx.rt.dispatch_pass(&spy));
    assert_eq!(missing, Some((T.to_base(), QH.into())));
    assert_eq!(lines.iter().filter(|l| l.contains(MISSING)).count(), 1, "{lines:?}");
    assert!(spy.sizes().is_empty());
    assert_eq!(pending(&fx), 40 - 32);
    assert_eq!(aborts(&fx), 0);
}

/// Signs as the operator and records whether S was free.
struct SignProbe {
    rt: OnceLock<Weak<Rt>>,
    s_free: Mutex<Vec<bool>>,
}

impl OperatorSigner for SignProbe {
    fn sign(&self, hash: &[u8; 32], scheme: BlsSchemeKind) -> Option<BlsSigValue> {
        self.s_free.lock().unwrap().push(rt_of(&self.rt).is_some_and(|rt| s_free(&rt)));
        Some(sign(&sk(0), hash, scheme == BlsSchemeKind::Legacy))
    }
}

#[test]
fn a_sign_task_creates_its_share_with_s_free() {
    let mut params = quorum(1).params;
    params.size = 1;
    let single = Arc::new(FakeQuorum {
        params,
        ..Arc::into_inner(verifiable(1)).unwrap()
    });
    let probe = Arc::new(SignProbe {
        rt: OnceLock::new(),
        s_free: Mutex::default(),
    });
    let fx = fixture_with(single, 0, probe.clone());
    probe.rt.set(Arc::downgrade(&fx.rt)).unwrap();
    fx.rt.async_sign(fx.q.clone(), ID, MSG);
    fx.rt.dispatch_pass(&ReferenceVerifier);
    assert_eq!(*probe.s_free.lock().unwrap(), [true]);
    assert_eq!(
        effects(&fx),
        [Ev::Recovered {
            consider: true,
            s_free: true,
        }]
    );
}

#[test]
fn an_interrupt_stops_dispatching_signs_and_collecting() {
    let fx = fixture(3, 0);
    flood(&fx, 10);
    for i in 0..3 {
        fx.rt.async_sign(fx.q.clone(), id(i), MSG);
    }
    let (stop, mut pushed) = (Cell::new(false), Vec::new());
    fx.rt.dispatcher_pass(
        || stop.get(),
        |w| {
            pushed.push(matches!(w, Work::Sign(_)));
            stop.set(true);
        },
    );
    assert_eq!(pushed, [true]);
    assert_eq!(pending(&fx), 10);
}

/// Set once `passes` sleeps have been taken; records each sleep and each loop-head check.
struct Ticks {
    passes: usize,
    events: RefCell<Vec<Option<Duration>>>,
}

impl Ticks {
    fn new(passes: usize) -> Self {
        Self {
            passes,
            events: RefCell::default(),
        }
    }

    fn sleeps(&self) -> Vec<Duration> {
        self.events.borrow().iter().flatten().copied().collect()
    }
}

impl Pause for Ticks {
    fn is_set(&self) -> bool {
        self.events.borrow_mut().push(None);
        self.sleeps().len() >= self.passes
    }

    fn sleep_for(&self, d: Duration) -> bool {
        self.events.borrow_mut().push(Some(d));
        self.sleeps().len() < self.passes
    }
}

#[test]
fn maint_sleeps_100_ms_once_per_pass() {
    let fx = fixture(3, 0);
    let ticks = Ticks::new(3);
    fx.rt.maint_loop(&ticks);
    let (c, s) = (None, Some(Duration::from_millis(100)));
    assert_eq!(*ticks.events.borrow(), [c, s, c, s, c, s, c]);
}

#[test]
fn the_dispatcher_sleeps_10_ms_once_per_pass_whatever_the_work() {
    let fx = fixture(3, 0);
    flood(&fx, 200);
    let ticks = Ticks::new(3);
    let mut held = Vec::new();
    fx.rt.dispatch_loop(&ticks, |w| held.push(w));
    assert_eq!(ticks.sleeps(), [Duration::from_millis(10); 3]);
    assert_eq!((held.len(), pending(&fx)), (4, 200 - 128), "passes 2 and 3 had no room");

    drop(held);
    let spy = Spy::new(&fx.rt);
    let ticks = Ticks::new(2);
    fx.rt.dispatch_loop(&ticks, |w| run(&fx.rt, w, &spy));
    assert_eq!(ticks.sleeps(), [Duration::from_millis(10); 2]);
    assert_eq!(spy.sizes(), [32, 32, 8], "pass 2 had no work");
}

#[test]
fn an_interrupt_joins_every_thread_within_a_tick_in_core_order() {
    let fx = fixture(3, 0);
    flood(&fx, 10);
    let interrupt = SigningInterrupt::new();
    let threads = fx.rt.start(interrupt.clone(), || ReferenceVerifier).unwrap();
    let start = Instant::now();
    interrupt.trigger();
    let mut order = Vec::new();
    threads.join_with(|name| order.push(name));
    assert!(start.elapsed() < Duration::from_secs(10), "{:?}", start.elapsed());
    assert_eq!(order, ["sigsh-maint", "sigsh-dispat", "sigsh-work"]);
}

struct OnDrop(mpsc::Sender<()>);

impl Drop for OnDrop {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

#[test]
fn stopping_the_pool_drops_queued_tasks_unrun() {
    let pool = Pool::start(1, &no_panic()).unwrap();
    let (started, running) = mpsc::channel();
    let (dropped, released) = mpsc::channel();
    pool.queue().push(Box::new(move || {
        started.send(()).unwrap();
        let _ = released.recv_timeout(Duration::from_secs(5));
    }));
    let ran = Arc::new(AtomicBool::new(false));
    let (flag, guard) = (ran.clone(), OnDrop(dropped));
    pool.queue().push(Box::new(move || {
        let _guard = guard;
        flag.store(true, Ordering::SeqCst);
    }));
    running.recv().unwrap();
    pool.stop();
    assert!(!ran.load(Ordering::SeqCst));
}

#[test]
fn the_pool_has_core_s_size_and_names() {
    assert_eq!([1, 2, 8, 64].map(worker_count), [1, 1, 4, 4]);
    let pool = Pool::start(3, &no_panic()).unwrap();
    let (barrier, names) = (Arc::new(Barrier::new(3)), Arc::new(Mutex::new(BTreeSet::new())));
    let done = Arc::new(AtomicUsize::new(0));
    for _ in 0..3 {
        let (barrier, names, done) = (barrier.clone(), names.clone(), done.clone());
        pool.queue().push(Box::new(move || {
            barrier.wait();
            names.lock().unwrap().insert(thread::current().name().unwrap().to_owned());
            done.fetch_add(1, Ordering::SeqCst);
        }));
    }
    while done.load(Ordering::SeqCst) < 3 {
        thread::yield_now();
    }
    pool.stop();
    assert_eq!(
        *names.lock().unwrap(),
        BTreeSet::from(["sigsh-work.0", "sigsh-work.1", "sigsh-work.2"].map(String::from))
    );
}
