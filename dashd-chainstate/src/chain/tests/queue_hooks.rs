//! P3's signal contract on V (CH1-node §8.4, D67): C8's `NewBlock`, C5's hook at the idle point
//! and the release points, C5′'s wait before an accept outside IBD, A-4b's `merkle_checked` and
//! BL-236's per-view reads.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, unbounded};
use dash_consensus::{Block, BlockId, BlockValidationResult};
use dash_primitives::BlockHash;

use super::actor::{Log, entries, logged};
use super::failure::{flag_at_close, restartable, three_forks, tip};
use super::flush::misused_attach;
use super::support::{Ev, Mined, Rig, RigConfig, cb_script, child, coinbase};
use crate::chain::actor::tests::failed_wait;
use crate::chain::actor::{COINS_ATTACH_MISUSE, V};
use crate::chain::reply::reply;
use crate::chain::{
    BlockReply, BlockState, Blocking, ChainCommand, ChainHandle, Fatal, NewBlock, Queued,
    ServiceWait, ShutdownFlag as _, ValidationInterface, ValidationSignals, WaitError,
};

const SOON: Duration = Duration::from_secs(120);
/// How long a test waits to see that something has not happened.
const BRIEF: Duration = Duration::from_millis(300);

/// Out of IBD from the start: genesis is an hour old.
fn out_of_ibd() -> Rig {
    Rig::with(RigConfig {
        after_genesis: Some(3_600),
        ..RigConfig::default()
    })
}

/// A wait a test opens: `wait_until` on its wake, until it is open.
struct Latch {
    open: AtomicBool,
    wake: (Sender<()>, Receiver<()>),
}

impl Default for Latch {
    fn default() -> Latch {
        Latch {
            open: AtomicBool::new(true),
            wake: unbounded(),
        }
    }
}

impl Latch {
    fn close(&self) {
        self.open.store(false, Ordering::SeqCst);
    }

    fn open(&self) {
        self.open.store(true, Ordering::SeqCst);
        self.wake.0.send(()).expect("the latch holds its receiver");
    }

    fn wait(&self, wait: &mut dyn ServiceWait) -> Result<(), WaitError> {
        wait.wait_until(&self.wake.1, &mut || self.open.load(Ordering::SeqCst))
    }
}

/// P3's two hooks, each gated by its own latch, counting the calls and ringing `entered` when a
/// call finds its latch closed.
struct Gates {
    accept: Latch,
    release: Latch,
    accept_calls: AtomicUsize,
    release_calls: AtomicUsize,
    entered: (Sender<&'static str>, Receiver<&'static str>),
}

impl Default for Gates {
    fn default() -> Gates {
        Gates {
            accept: Latch::default(),
            release: Latch::default(),
            accept_calls: AtomicUsize::new(0),
            release_calls: AtomicUsize::new(0),
            entered: unbounded(),
        }
    }
}

impl Gates {
    fn entered(&self) -> &'static str {
        self.entered.1.recv_timeout(SOON).expect("V reached the hook")
    }
}

impl ValidationInterface for Gates {
    fn release_point(&self, wait: &mut dyn ServiceWait) -> Result<(), WaitError> {
        self.release_calls.fetch_add(1, Ordering::SeqCst);
        if !self.release.open.load(Ordering::SeqCst) {
            self.entered.0.send("release").expect("the test listens");
        }
        self.release.wait(wait)
    }

    fn before_accept(&self, wait: &mut dyn ServiceWait) -> Result<(), WaitError> {
        self.accept_calls.fetch_add(1, Ordering::SeqCst);
        if !self.accept.open.load(Ordering::SeqCst) {
            self.entered.0.send("accept").expect("the test listens");
        }
        self.accept.wait(wait)
    }
}

/// Whether V has `m`'s data, read on V: a class R command, so a wait serves it.
fn stored(v: &ChainHandle, m: &Mined) -> Option<bool> {
    let h = m.hash();
    v.query(move |q| {
        let r = q.chain.reader();
        r.lookup(&h)
            .and_then(|id| r.entry(id))
            .is_some_and(|e| e.status.has(dash_chain::Status::HAVE_DATA))
    })
    .recv_timeout(SOON)
    .and_then(Result::ok)
}

#[derive(Debug, PartialEq, Eq)]
enum Seen {
    New {
        id: BlockId,
        hash: BlockHash,
        height: u32,
        stamp: u64,
        found: Option<BlockId>,
        parent: Option<BlockId>,
    },
    /// `NotifyHeaderTip` follows the accept's `scope_end` (`v:4903`): the `view_seq` then
    /// published.
    Published(u64),
}

#[derive(Default)]
struct Stamps {
    v: OnceLock<ChainHandle>,
    log: Mutex<Vec<Seen>>,
}

impl ValidationInterface for Stamps {
    fn new_pow_valid_block(&self, nb: &NewBlock<'_>) {
        let seen = Seen::New {
            id: nb.id,
            hash: nb.entry.hash,
            height: nb.entry.height,
            stamp: nb.stamp,
            found: nb.tree.lookup(&nb.block.hash()),
            parent: nb.tree.ancestor(nb.id, nb.entry.height - 1),
        };
        self.log.lock().expect("log").push(seen);
    }

    fn notify_header_tip(&self, _: BlockId, _: bool) {
        let seq = self.v.get().expect("set").view().view_seq;
        self.log.lock().expect("log").push(Seen::Published(seq));
    }
}

/// C8: `NewPoWValidBlock` carries the entry, the stamp the accept's `scope_end` then publishes,
/// and a tree that finds the header this same accept inserted, and its parent.
#[test]
fn new_block_carries_the_stamp_and_the_in_scope_tree() {
    let rig = out_of_ibd();
    let stamps = Arc::new(Stamps::default());
    assert!(stamps.v.set(rig.v.clone()).is_ok());
    rig.signals.register(stamps.clone());
    let a = child(&rig.p, &rig.genesis, 0);
    rig.accept(&a);
    let (ai, gi) = (rig.id(&a), rig.id(&rig.genesis));
    let log = std::mem::take(&mut *stamps.log.lock().expect("log"));
    let [
        Seen::New {
            stamp,
            ..
        },
        Seen::Published(published),
    ] = log[..]
    else {
        panic!("{log:?}")
    };
    assert_eq!(stamp, published, "the stamp is the view the accept's scope_end stores");
    assert_eq!(
        log[0],
        Seen::New {
            id: ai,
            hash: a.hash(),
            height: 1,
            stamp,
            found: Some(ai),
            parent: Some(gi),
        }
    );
}

/// V gates the signals it builds arguments for on a registered listener (D67).
#[test]
fn has_listeners_follows_registration() {
    struct Quiet;
    impl ValidationInterface for Quiet {}
    let signals = ValidationSignals::default();
    assert!(!signals.has_listeners());
    let l: Arc<dyn ValidationInterface> = Arc::new(Quiet);
    signals.register(l.clone());
    signals.register(l.clone());
    assert!(signals.has_listeners());
    signals.unregister(&l);
    assert!(!signals.has_listeners());
}

/// A-4b: `merkle_checked` skips `CheckBlock`'s merkle pass and nothing else, as Core's
/// `m_checked_merkle_root` (`v:4332`). A mutated block then fails the next check instead.
#[test]
fn merkle_checked_skips_only_the_merkle_pass() {
    let rig = Rig::new();
    let a = child(&rig.p, &rig.genesis, 0);
    let txs = vec![a.block.txs()[0].clone(), coinbase(&cb_script(1, 9))];
    let mutated = Arc::new(Block::new(a.header(), txs));
    let verdict = |merkle_checked| {
        let r = rig.v.submit_block(mutated.clone(), true, None, true, merkle_checked);
        let r = r.recv().expect("V replies");
        let BlockState::Invalid(e) = r.state else {
            panic!("{r:?}")
        };
        (e.result(), e.reject_reason().to_owned())
    };
    assert_eq!(verdict(false).0, BlockValidationResult::Mutated);
    assert_eq!(verdict(true), (BlockValidationResult::Consensus, "bad-cb-multiple".to_owned()));
    assert!(rig.v.reader().lookup(&a.hash()).is_none());
    rig.accept(&a);
}

/// BL-236: `contains` and `at` answer at the view given, not the live chain.
#[test]
fn contains_and_at_answer_at_the_view() {
    let rig = Rig::new();
    let vh = rig.v.view_handle();
    let before = vh.view();
    let a = child(&rig.p, &rig.genesis, 0);
    rig.accept(&a);
    let after = vh.view();
    let (ai, gi) = (rig.id(&a), rig.id(&rig.genesis));
    assert!(!vh.contains(&before, ai) && vh.contains(&before, gi));
    assert_eq!((vh.at(&before, 0), vh.at(&before, 1)), (Some(gi), None));
    assert!(vh.contains(&after, ai));
    assert_eq!(vh.at(&after, 1), Some(ai));
}

/// C5′ (DEC-240): outside IBD, an accept waits in `before_accept`. The wait serves a class R
/// command, defers a second `ProcessBlock` whole (it neither runs nor waits there), and when it
/// ends the first accept runs, then the second, which passes the check again.
#[test]
fn an_accept_outside_ibd_waits_and_defers_the_next_one() {
    let rig = out_of_ibd();
    let gates = Arc::new(Gates::default());
    rig.signals.register(gates.clone());
    let a = child(&rig.p, &rig.genesis, 0);
    let b = child(&rig.p, &a, 0);
    gates.accept.close();
    let ra = rig.v.process_block(a.block.clone(), true, None, true);
    assert_eq!(gates.entered(), "accept");
    assert_eq!(stored(&rig.v, &a), Some(false), "a query is served while the accept waits");
    let rb = rig.v.process_block(b.block.clone(), true, None, true);
    assert_eq!(stored(&rig.v, &b), Some(false), "b is deferred whole");
    assert_eq!(gates.accept_calls.load(Ordering::SeqCst), 1);
    assert!(ra.recv_timeout(BRIEF).is_none(), "a waits");
    gates.accept.open();
    assert!(ra.recv_timeout(SOON).expect("a's reply").expect("accepted").accepted);
    assert!(rb.recv_timeout(SOON).expect("b's reply").expect("accepted").accepted);
    assert_eq!(gates.accept_calls.load(Ordering::SeqCst), 2, "b passed the check after the wait");
    let (ai, bi) = (rig.id(&a), rig.id(&b));
    let headers: Vec<_> =
        rig.rec.take().into_iter().filter(|e| matches!(e, Ev::Header(_))).collect();
    assert_eq!(headers, [Ev::Header(ai), Ev::Header(bi)]);
    assert_eq!(tip(&rig), bi);
    rig.assert_no_abort();
}

/// In IBD V emits no `NewPoWValidBlock` (`v:4837`), so C5′ does not apply: a closed accept gate
/// is never reached. C5's idle point still runs.
#[test]
fn no_accept_wait_in_ibd() {
    let rig = Rig::new();
    let gates = Arc::new(Gates::default());
    rig.signals.register(gates.clone());
    gates.accept.close();
    rig.accept(&child(&rig.p, &rig.genesis, 0));
    assert_eq!(gates.accept_calls.load(Ordering::SeqCst), 0);
    assert!(gates.release_calls.load(Ordering::SeqCst) > 0, "the idle point calls C5's hook");
    let pow_valid: Vec<_> =
        rig.rec.take().into_iter().filter(|e| matches!(e, Ev::PowValid(_))).collect();
    assert_eq!(pow_valid, [], "C8 skips IBD (Opus r1 M26)");
}

/// A-1b (3): at the idle point V waits with no class C operation running. It serves class R
/// commands, defers class C ones (their replies follow the wait) and runs no release point.
#[test]
fn the_idle_point_serves_r_and_defers_c() {
    let rig = Rig::new();
    let gates = Arc::new(Gates::default());
    rig.signals.register(gates.clone());
    gates.release.close();
    // V is parked in its idle select: a query wakes it, and the next idle point waits.
    assert_eq!(stored(&rig.v, &rig.genesis), Some(true));
    assert_eq!(gates.entered(), "release");
    let abc = rig.v.activate_best_chain();
    assert_eq!(stored(&rig.v, &rig.genesis), Some(true), "a query is served while V waits");
    assert!(abc.recv_timeout(BRIEF).is_none(), "the ABC is deferred");
    gates.release.open();
    assert_eq!(abc.recv_timeout(SOON), Some(Ok(Ok(()))));
    rig.assert_no_abort();
}

/// D38: while V holds commands for startup's ABC it calls no hook, at the idle point or at that
/// ABC's release points: each call it makes after sees the reorged tip, b2.
#[test]
fn no_hook_while_commands_wait_for_startups_abc() {
    #[derive(Default)]
    struct Tips {
        v: OnceLock<ChainHandle>,
        seen: Mutex<Vec<Option<BlockId>>>,
    }
    impl ValidationInterface for Tips {
        fn release_point(&self, _: &mut dyn ServiceWait) -> Result<(), WaitError> {
            let tip = self.v.get().map(|v| v.view().snap.tip.tip);
            self.seen.lock().expect("seen").push(tip);
            Ok(())
        }
    }
    let (mut rig, files) = restartable();
    let [main, side, _] = three_forks(&rig, 2);
    let (a2, b2) = (rig.id(&main[1]), rig.id(&side[1]));
    assert_eq!(rig.v.invalidate_block(main[0].hash()).recv(), Ok(Ok(())));
    let exit = rig.stop();
    let tips = Arc::new(Tips::default());
    let set = tips.clone();
    let cfg = RigConfig {
        listener: Some(tips.clone()),
        before_init: Some(Box::new(move |v: &ChainHandle| {
            assert!(set.v.set(v.clone()).is_ok());
        })),
        ..RigConfig::default()
    };
    let mut rig = Rig::restart_with(exit, Box::new(files), a2, cfg).expect("V starts");
    assert_eq!(tip(&rig), b2);
    // A query V serves after its next idle point.
    assert_eq!(stored(&rig.v, &side[1]), Some(true));
    let seen = tips.seen.lock().expect("seen").clone();
    assert!(!seen.is_empty(), "the idle point after startup's ABC calls the hook");
    assert!(seen.iter().all(|&t| t == Some(b2)), "{seen:?}");
    assert!(rig.stop().fatal.is_none());
}

/// D67 R2 (Opus r1 B1): a wait serves held commands first, as release points do (D38). With a
/// `ProcessBlock` outside IBD, a query and an ABC held, the accept's C5′ wait serves the held query
/// before a later lane one and defers the held ABC, which runs once the wait ends. V calls no C5
/// hook before the accept: the idle point skips while commands are held.
#[test]
fn a_wait_serves_held_commands_first() {
    let rig = out_of_ibd();
    let gates = Arc::new(Gates::default());
    rig.signals.register(gates.clone());
    let log = Log::default();
    let a = child(&rig.p, &rig.genesis, 0);
    let (to_a, accepted) = reply();
    let (to_abc, activated) = reply();
    let (block, query) = (a.block.clone(), logged(&log, "held"));
    let (g, before) = (gates.clone(), Arc::new(AtomicUsize::new(usize::MAX)));
    let staged = before.clone();
    gates.accept.close();
    rig.on_v(move |vv| {
        vv.stage_held(ChainCommand::ProcessBlock {
            hash: block.hash(),
            block,
            force: true,
            from: None,
            merkle_checked: false,
            reply: Some(BlockReply::Activated(to_a)),
        });
        vv.stage_held(query);
        vv.stage_held(ChainCommand::ActivateBestChain {
            reply: Some(to_abc),
        });
        staged.store(g.release_calls.load(Ordering::SeqCst), Ordering::SeqCst);
        Ok(())
    });
    assert_eq!(gates.entered(), "accept");
    let calls = gates.release_calls.load(Ordering::SeqCst);
    assert_eq!(calls, before.load(Ordering::SeqCst), "no idle point while commands are held");
    rig.v.send(logged(&log, "lane")).expect("V runs");
    assert_eq!(stored(&rig.v, &a), Some(false), "served at the accept's wait");
    assert_eq!(entries(&log), ["held", "lane"], "the held query first");
    assert!(activated.recv_timeout(BRIEF).is_none(), "the held ABC is deferred");
    gates.accept.open();
    assert!(accepted.recv_timeout(SOON).expect("a's reply").expect("accepted").accepted);
    assert_eq!(activated.recv_timeout(SOON), Some(Ok(Ok(()))));
    assert_eq!(tip(&rig), rig.id(&a));
    rig.assert_no_abort();
}

/// The first tip [`Backlog`] holds the signals thread on.
const HOLD_TIP: u32 = 900_000;

/// Holds the signals thread on [`HOLD_TIP`] until the test releases it, and records
/// `callbacks_pending()` at each C5 hook call V makes while `watch` is set.
struct Backlog {
    signals: ValidationSignals,
    go: Mutex<Receiver<()>>,
    inside: AtomicBool,
    watch: AtomicBool,
    pending: Mutex<Vec<usize>>,
}

impl Backlog {
    /// Registers one on `rig`. Each send releases one hold; dropping the sender releases all.
    fn register(rig: &Rig) -> (Arc<Backlog>, Sender<()>) {
        let (go, gate) = unbounded();
        let b = Arc::new(Backlog {
            signals: rig.signals.clone(),
            go: Mutex::new(gate),
            inside: AtomicBool::new(false),
            watch: AtomicBool::new(false),
            pending: Mutex::default(),
        });
        rig.signals.register(b.clone());
        (b, go)
    }

    /// Holds the signals thread with eleven entries pending behind it: over Core's limit of 10.
    fn hold(&self, rig: &Rig) {
        self.inside.store(false, Ordering::SeqCst);
        for i in 0..12 {
            rig.signals.enqueue(Queued::UpdatedBlockTip {
                new: BlockId(HOLD_TIP + i),
                fork: None,
                ibd: false,
            });
        }
    }

    /// Until V has queued its barrier behind the held thread: V waits in `limit_queue`.
    fn until_v_waits(&self) {
        let deadline = Instant::now() + SOON;
        while !(self.inside.load(Ordering::SeqCst) && self.signals.callbacks_pending() > 11) {
            assert!(Instant::now() < deadline, "V never reached the queue limit");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn unregister(self: Arc<Self>, rig: &Rig) {
        rig.signals.unregister(&(self as Arc<dyn ValidationInterface>));
    }
}

impl ValidationInterface for Backlog {
    fn updated_block_tip(&self, new: BlockId, _: Option<BlockId>, _: bool) {
        if new.0 == HOLD_TIP {
            self.inside.store(true, Ordering::SeqCst);
            let _ = self.go.lock().expect("go").recv();
        }
    }

    fn release_point(&self, _: &mut dyn ServiceWait) -> Result<(), WaitError> {
        if self.watch.load(Ordering::SeqCst) {
            self.pending.lock().expect("pending").push(self.signals.callbacks_pending());
        }
        Ok(())
    }
}

/// D67 R4 (Opus r1 N6, M5): at both of Core's `LimitValidationInterfaceQueue` sites, ABC's loop
/// head (`v:3766`) and `InvalidateBlock`'s (`v:3946`), C5's hook follows the queue limit: each
/// call there finds the queue within Core's 10.
#[test]
fn c5s_hook_follows_the_queue_limit_at_both_sites() {
    let rig = Rig::new();
    let a = child(&rig.p, &rig.genesis, 0);
    rig.accept(&a);
    rig.sync();
    let (backlog, go) = Backlog::register(&rig);
    let id = rig.id(&a);
    type Site = fn(&mut V, BlockId) -> Result<(), Fatal>;
    let sites: [(&str, Site); 2] = [
        ("abc", |v, _| v.activate_best_chain(None)),
        ("invalidate", |v, id| v.invalidate_block(id).map(|_| ())),
    ];
    for (site, run) in sites {
        backlog.hold(&rig);
        let (done, waited) = reply();
        let b = backlog.clone();
        rig.v
            .send(ChainCommand::TestOnV(Box::new(move |v| {
                b.watch.store(true, Ordering::SeqCst);
                let r = run(v, id);
                b.watch.store(false, Ordering::SeqCst);
                done.send(());
                r
            })))
            .expect("sent");
        backlog.until_v_waits();
        assert!(waited.try_recv().is_none(), "{site}: V waits at the limit");
        go.send(()).expect("the backlog holds");
        waited.recv_timeout(SOON).expect("the wait ends").expect("ran");
        let pending = std::mem::take(&mut *backlog.pending.lock().expect("pending"));
        assert!(!pending.is_empty() && pending.iter().all(|&n| n <= 10), "{site}: {pending:?}");
    }
    assert_eq!(tip(&rig), rig.id(&rig.genesis), "a is invalid");
    backlog.unregister(&rig);
    rig.assert_no_abort();
}

/// Sends V a command that fails with an `AbortNode` and waits for V to stop on it in the wait it
/// is in. V serves the command at once, so its reply may close before it could be polled.
fn abort_in_the_wait(rig: &Rig) {
    let misused = misused_attach(&rig.v);
    assert_eq!(misused.recv_timeout(SOON), Some(Err(WaitError::Disconnected)));
    assert!(rig.shutdown.shutdown_requested(), "V stopped in the wait");
}

/// CH3b D213 (b) at the queue wait (`v:3766`): an `AbortNode` from a command V serves while ABC
/// waits in `LimitValidationInterfaceQueue` stops V there. The wait ends and serves nothing
/// more, ABC takes its step (a1 connects and its reply arrives) and breaks at `v:3847`, and V's
/// loop then ends on the command's `AbortNode`, reported once.
#[test]
fn an_abort_at_abcs_queue_wait_lets_abc_take_its_step() {
    let mut rig = Rig::new();
    let (backlog, go) = Backlog::register(&rig);
    let a1 = child(&rig.p, &rig.genesis, 0);
    backlog.hold(&rig);
    let processed =
        flag_at_close(rig.v.process_block(a1.block.clone(), true, None, true), &rig.shutdown);
    backlog.until_v_waits();
    abort_in_the_wait(&rig);
    let (accepted, flag) = processed();
    assert!(accepted.expect("a1's reply").accepted);
    assert_eq!(flag, Some(true), "V had stopped before ABC's step");
    assert_eq!(tip(&rig), rig.id(&a1), "the step connected a1");
    drop(go);
    backlog.unregister(&rig);
    rig.stop_on(COINS_ATTACH_MISUSE);
}

/// CH3b D213: outside a class C operation a command's `AbortNode` at a wait is V's own. At C5′'s
/// wait before an accept it ends the wait and V stops at once: the accept that waited never runs,
/// and its reply disconnects after the node's flag.
#[test]
fn an_abort_at_the_accept_wait_stops_v_before_the_accept() {
    let mut rig = out_of_ibd();
    let gates = Arc::new(Gates::default());
    rig.signals.register(gates.clone());
    let a = child(&rig.p, &rig.genesis, 0);
    gates.accept.close();
    let ra = flag_at_close(rig.v.process_block(a.block.clone(), true, None, true), &rig.shutdown);
    assert_eq!(gates.entered(), "accept");
    abort_in_the_wait(&rig);
    assert_eq!(ra(), (Err(WaitError::Disconnected), Some(true)), "a was never accepted");
    assert_eq!(rig.v.reader().lookup(&a.hash()), None, "nor its header");
    assert_eq!(rig.stop().fatal.map(|f| f.0).as_deref(), Some(COINS_ATTACH_MISUSE));
}

/// R3 at the signals' barrier wait (Opus r1 N6): outside IBD, a `ProcessBlock` that reaches V while
/// ABC waits in `LimitValidationInterfaceQueue` (`v:3766`) is deferred whole. It meets C5′ only
/// after the ABC, and its reply follows.
#[test]
fn an_accept_met_at_the_queue_limit_is_deferred_whole() {
    let rig = out_of_ibd();
    let gates = Arc::new(Gates::default());
    rig.signals.register(gates.clone());
    rig.sync();
    let (backlog, go) = Backlog::register(&rig);
    let a = child(&rig.p, &rig.genesis, 0);
    backlog.hold(&rig);
    let abc = rig.v.activate_best_chain();
    backlog.until_v_waits();
    let ra = rig.v.process_block(a.block.clone(), true, None, true);
    assert_eq!(stored(&rig.v, &a), Some(false), "a is deferred whole: not even its header");
    assert_eq!(gates.accept_calls.load(Ordering::SeqCst), 0);
    assert!(ra.recv_timeout(BRIEF).is_none(), "a waits for the ABC");
    go.send(()).expect("the backlog holds");
    assert_eq!(abc.recv_timeout(SOON), Some(Ok(Ok(()))));
    assert!(ra.recv_timeout(SOON).expect("a's reply").expect("accepted").accepted);
    assert_eq!(gates.accept_calls.load(Ordering::SeqCst), 1, "a met C5′ after the ABC");
    backlog.unregister(&rig);
    rig.assert_no_abort();
}

/// `wait_until`'s closed `wake` (Opus r1 N6, M9): once the sender is gone the wait ends,
/// `Disconnected` unless `done` holds.
#[test]
fn a_closed_wake_ends_wait_until() {
    let (tx, wake) = unbounded::<()>();
    drop(tx);
    assert_eq!(Blocking.wait_until(&wake, &mut || false), Err(WaitError::Disconnected));
    assert_eq!(Blocking.wait_until(&wake, &mut || true), Ok(()));
    let mut calls = 0;
    let mut done_on_the_second_check = || {
        calls += 1;
        calls > 1
    };
    assert_eq!(Blocking.wait_until(&wake, &mut done_on_the_second_check), Ok(()));
}

/// The same on V: a listener whose sender goes while V waits in its hook gets `Disconnected`,
/// and V runs the accept on.
#[test]
fn a_listeners_closed_wake_ends_vs_wait() {
    struct Closing {
        wake: Mutex<Option<Sender<()>>>,
        rx: Receiver<()>,
        entered: Sender<()>,
        results: Mutex<Vec<Result<(), WaitError>>>,
    }
    impl ValidationInterface for Closing {
        fn before_accept(&self, wait: &mut dyn ServiceWait) -> Result<(), WaitError> {
            self.entered.send(()).expect("the test listens");
            let r = wait.wait_until(&self.rx, &mut || false);
            self.results.lock().expect("results").push(r);
            r
        }
    }
    let rig = out_of_ibd();
    let (tx, rx) = unbounded();
    let (entered, inside) = unbounded();
    let closing = Arc::new(Closing {
        wake: Mutex::new(Some(tx)),
        rx,
        entered,
        results: Mutex::default(),
    });
    rig.signals.register(closing.clone());
    let a = child(&rig.p, &rig.genesis, 0);
    let ra = rig.v.process_block(a.block.clone(), true, None, true);
    inside.recv_timeout(SOON).expect("V reached the hook");
    assert!(ra.recv_timeout(BRIEF).is_none(), "V waits");
    drop(closing.wake.lock().expect("wake").take());
    assert!(ra.recv_timeout(SOON).expect("a's reply").expect("accepted").accepted);
    assert_eq!(*closing.results.lock().expect("results"), [Err(WaitError::Disconnected)]);
    rig.assert_no_abort();
}

/// Opus r1 N1: a wait that has recorded an `AbortNode` serves nothing more if the listener waits
/// again, and returns `Disconnected` at once.
#[test]
fn a_failed_wait_serves_nothing_more() {
    let rig = Rig::new();
    let log = Log::default();
    let (v, query) = (rig.v.clone(), logged(&log, "queued"));
    let (_tx, wake) = unbounded::<()>();
    let seen = log.clone();
    rig.on_v(move |vv| {
        v.send(query).expect("V runs");
        let mut checks = 0;
        let mut wait = failed_wait(vv, Fatal::new("disk on fire"));
        let r = wait.wait_until(&wake, &mut || {
            checks += 1;
            checks > 1
        });
        assert_eq!((r, checks), (Err(WaitError::Disconnected), 0));
        assert!(seen.lock().expect("log").is_empty(), "nothing served");
        Ok(())
    });
    rig.v.query(|_| ()).recv_timeout(SOON).expect("V serves on").expect("served");
    assert_eq!(entries(&log), ["queued"]);
    rig.assert_no_abort();
}
