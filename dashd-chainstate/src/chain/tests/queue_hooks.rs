//! P3's signal contract on V (CH1-node §8.4, D67): C8's `NewBlock`, C5's hook at the idle point
//! and the release points, C5′'s wait before an accept outside IBD, A-4b's `merkle_checked` and
//! BL-236's per-view reads.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, unbounded};
use dash_consensus::{Block, BlockId, BlockValidationResult};
use dash_primitives::BlockHash;
use dashd_store::ErrorKind;

use super::actor::{Log, entries, logged};
use super::connect::{Admitting, Latched, W_STORE_TEXT, WSide, admitted, exit_tip, w_fails};
use super::failure::{at_close, flag_at_close, restartable, three_forks, tip};
use super::flush::misused_attach;
use super::support::{Ev, Mined, Rig, RigConfig, batch, cb_script, child, coinbase};
use crate::chain::actor::tests::failed_wait;
use crate::chain::actor::{COINS_ATTACH_MISUSE, V};
use crate::chain::reply::reply;
use crate::chain::{
    AbortCause, AbortSink, BlockReply, BlockResult, BlockState, Blocking, ChainCommand,
    ChainHandle, Fatal, FlushBell, FlushHost, FlushSite, FlushStateMode, NewBlock, Queued,
    RecordingAbort, ServiceWait, ShutdownFlag as _, SiteDecision, ValidationInterface,
    ValidationSignals, WaitError,
};
use crate::store::{Failure, GlobalFailStop};

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
    /// `NotifyHeaderTip` follows the accept's `scope_end` (`v:4908` at v24.0.0): the `view_seq`
    /// then published.
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
/// `m_checked_merkle_root` (`v:4337` at v24.0.0). A mutated block then fails the next check
/// instead.
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
/// head (`v:3771` at v24.0.0) and `InvalidateBlock`'s (`v:3951` at v24.0.0), C5's hook follows the
/// queue limit: each call there finds the queue within Core's 10.
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
        ("invalidate", |v, id| v.invalidate_block(id, &mut false).map(|_| ())),
    ];
    for (site, run) in sites {
        backlog.hold(&rig);
        let (done, waited) = reply();
        let b = backlog.clone();
        rig.v
            .send(ChainCommand::TestUnderV(Box::new(move |v| {
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

/// CH3b D213 (b) at the queue wait (`v:3771` at v24.0.0): an `AbortNode` from a command V serves
/// while ABC waits in `LimitValidationInterfaceQueue` stops V there. The wait ends and serves
/// nothing more, ABC takes its step (a1 connects and its reply arrives) and breaks at `v:3852` at
/// v24.0.0, and V's loop then ends on the command's `AbortNode`, reported once.
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
/// ABC waits in `LimitValidationInterfaceQueue` (`v:3771` at v24.0.0) is deferred whole. It meets
/// C5′ only after the ABC, and its reply follows.
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

/// A BL-678 rig on W's side `w`: a1 accepted then invalidated, so an ABC after a1's reconsider
/// has a step to take; out of IBD when `out`.
fn reconsiderable(w: &WSide, out: bool) -> (Rig, Mined) {
    let rig = Rig::with(w.config(RigConfig {
        after_genesis: out.then_some(3_600),
        ..RigConfig::default()
    }));
    let a1 = invalidated_a1(&rig);
    (rig, a1)
}

/// a1 accepted on `rig`, then invalidated.
fn invalidated_a1(rig: &Rig) -> Mined {
    let a1 = child(&rig.p, &rig.genesis, 0);
    rig.accept(&a1);
    assert_eq!(rig.v.invalidate_block(a1.hash()).recv(), Ok(Ok(())));
    rig.sync();
    a1
}

/// What a BL-678 operation under a release point saw as it ended: its result's text, and whether
/// nothing was carried past.
type Unwound = (Result<(), String>, bool);

/// BL-678's nested wait: a class C operation on V reconsiders `a1` and runs ABC, whose release
/// point serves b's `ProcessBlock` (b on genesis, out of IBD); its C5′ wait holds at a closed
/// accept gate. Returns once V waits there: b's reply, and what the operation saw as it ended.
fn waiting_under_a_release_point(
    rig: &Rig,
    a1: &Mined,
) -> (crate::chain::ActorReply<BlockResult>, Receiver<Unwound>) {
    waiting_at_abcs_release_point(rig, Some(rig.id(a1)))
}

/// [`waiting_under_a_release_point`], the operation reconsidering `reconsider` if any first.
fn waiting_at_abcs_release_point(
    rig: &Rig,
    reconsider: Option<BlockId>,
) -> (crate::chain::ActorReply<BlockResult>, Receiver<Unwound>) {
    let gates = Arc::new(Gates::default());
    rig.signals.register(gates.clone());
    let b = child(&rig.p, &rig.genesis, 1);
    gates.accept.close();
    let (inside, entered) = unbounded();
    let (go, gone) = unbounded::<()>();
    let (seen, ended) = unbounded();
    let operation = ChainCommand::TestUnderV(Box::new(move |vv| {
        if let Some(id) = reconsider {
            vv.reconsider_block(id, false)?;
        }
        inside.send(()).expect("the test waits");
        gone.recv().expect("the test queues b");
        let r = vv.activate_best_chain(None);
        let _ = seen.send((r.clone().map_err(|f| f.0), vv.interrupted.is_none()));
        r
    }));
    rig.v.send(operation).expect("V runs");
    entered.recv_timeout(SOON).expect("V is inside the operation");
    let rb = rig.v.process_block(b.block.clone(), true, None, true);
    go.send(()).expect("V waits");
    assert_eq!(gates.entered(), "accept");
    (rb, ended)
}

/// What a BL-678 operation saw as it ended: its result's text, whether nothing was carried
/// past, and V's lane selects inside it.
type Ended = (Result<(), String>, bool, u32);

/// A class C operation on V that reconsiders `a1` and stages `held` if any, then waits for the
/// test's go, waits in `LimitValidationInterfaceQueue` and runs ABC: what it saw as it ended.
fn queue_wait_then_abc(
    rig: &Rig,
    a1: BlockId,
    held: Option<ChainCommand>,
) -> (Sender<()>, Receiver<Ended>) {
    let (inside, entered) = unbounded();
    let (go, gone) = unbounded::<()>();
    let (seen, ended) = unbounded();
    let operation = ChainCommand::TestUnderV(Box::new(move |vv| {
        vv.reconsider_block(a1, false)?;
        if let Some(cmd) = held {
            vv.stage_held(cmd);
        }
        inside.send(()).expect("the test waits");
        gone.recv().expect("the test goes on");
        crate::chain::actor::tests::LANE_SELECTS.with(|n| n.set(0));
        let r = vv.limit_queue().and_then(|()| vv.activate_best_chain(None));
        let selects = crate::chain::actor::tests::LANE_SELECTS.with(std::cell::Cell::get);
        let _ = seen.send((r.clone().map_err(|f| f.0), vv.interrupted.is_none(), selects));
        r
    }));
    rig.v.send(operation).expect("V runs");
    entered.recv_timeout(SOON).expect("V is inside the operation");
    (go, ended)
}

/// BL-678, paths 4h and 4l: inside ABC's queue wait (`v:3771`, the signals thread held past
/// Core's limit), V admits neither a held command nor the lane command its select takes once
/// W's failure has latched. It waits for W's report at the admission, then the wait ends on
/// the refusal, which unwinds ABC before its step (a1 stays unconnected), nothing carried past.
/// The command's reply disconnects; the node has W's one report. With `lane`, no bell is
/// attached and the signals thread is held to the end, so the lane arm is the one that won.
// Core: validation.cpp:3727-3733, :3771; shutdown.cpp:27-38
fn a_queue_wait_after_ws_failure_serves_nothing(lane: bool) {
    let w = WSide::new();
    let (mut rig, a1) = reconsiderable(&w, false);
    let (id, v) = (rig.id(&a1), rig.v_thread());
    let (backlog, hold) = Backlog::register(&rig);
    backlog.hold(&rig);
    let release = w.held.arm();
    let (to_held, held) = reply();
    let staged = (!lane).then(|| ChainCommand::ProcessHeaders {
        batch: batch(std::slice::from_ref(&a1)),
        from: None,
        reply: to_held,
    });
    let (go, ended) = queue_wait_then_abc(&rig, id, staged);
    let queued = if lane {
        go.send(()).expect("V waits");
        backlog.until_v_waits();
        drop(w.store_fails());
        Some(rig.v.process_headers(batch(std::slice::from_ref(&a1)), None))
    } else {
        drop(w.store_fails());
        go.send(()).expect("V waits");
        None
    };
    let parked = w.held.parked(&w.latch, v);
    let early = ended.try_recv().is_ok() || rig.shutdown.shutdown_requested();
    release.release();
    assert!(parked.is_some(), "lane {lane}: V waits for W's report at the admission");
    assert!(!early, "lane {lane}: the wait ended before W's report");
    let (r, uncarried, selects) = ended.recv_timeout(SOON).expect("the operation ends");
    assert_eq!(r, Err(W_STORE_TEXT.to_owned()), "lane {lane}");
    assert!(uncarried, "lane {lane}: nothing carried past");
    if lane {
        assert!(selects >= 1, "the lane arm took the command");
    }
    match queued {
        Some(r) => assert_eq!(r.recv().map(|_| ()), Err(WaitError::Disconnected)),
        None => assert_eq!(held.recv().map(|_| ()), Err(WaitError::Disconnected)),
    }
    drop(hold);
    let exit = rig.stop();
    backlog.unregister(&rig);
    assert_eq!(exit.fatal.as_ref().map(|f| f.0.as_str()), Some(W_STORE_TEXT), "lane {lane}");
    assert_eq!(exit_tip(&exit), Some(rig.genesis.hash()));
    assert_eq!(w.sink.messages(), [W_STORE_TEXT], "lane {lane}");
}

#[test]
fn a_held_command_at_a_queue_wait_after_ws_failure_is_refused() {
    a_queue_wait_after_ws_failure_serves_nothing(false);
}

#[test]
fn a_lane_command_at_a_queue_wait_after_ws_failure_is_refused() {
    a_queue_wait_after_ws_failure_serves_nothing(true);
}

/// BL-678, C3 at path 6: a stop carried past inside a BeforeAccept wait, its failure tripping
/// the latch itself (BL-535's shape), is not refused again at the accept's admission. a1 is the
/// tip; an ABC with no step to take serves b's `ProcessBlock` (a1's sibling, no more work) at its
/// release point, and b waits in its C5′ wait (a closed accept gate). A lane command served in
/// that wait fails with a store failure that reports and trips the latch. V carries past it, so
/// ABC goes on to its no-work break and returns `Ok`, as Core's does past another thread's
/// `AbortNode`, and the node has that one report.
// Core: validation.cpp:3771, :3789-3790, :4911-4914; shutdown.cpp:27-38
#[test]
fn a_carried_stop_in_a_wait_under_a_release_point_is_not_refused_at_the_accept() {
    let w = WSide::new();
    let mut rig = Rig::with(w.config(RigConfig {
        after_genesis: Some(3_600),
        ..RigConfig::default()
    }));
    let a1 = child(&rig.p, &rig.genesis, 0);
    rig.accept(&a1);
    rig.sync();
    let (rb, ended) = waiting_at_abcs_release_point(&rig, None);
    let (latch, sink) = (w.latch.clone(), w.sink.clone());
    let store_fails = ChainCommand::TestUnderMain(Box::new(move |_| {
        sink.abort_node(CARRIED_TEXT, None);
        assert!(latch.trip(Failure {
            kind: ErrorKind::NoSpace,
            site: "flush",
            message: CARRIED_TEXT.into(),
        }));
        Err(Fatal::reported(CARRIED_TEXT))
    }));
    rig.v.send(store_fails).expect("V waits");
    let (r, uncarried) = ended.recv_timeout(SOON).expect("the operation ends");
    assert_eq!(r, Ok(()), "ABC has no step to take");
    assert!(!uncarried, "the store failure was carried past");
    let b = rb.recv().expect("b's accept and tail ran");
    assert!(b.accepted && b.new_block, "{b:?}");
    let exit = rig.stop();
    assert_eq!(exit.fatal.as_ref().map(|f| f.0.as_str()), Some(CARRIED_TEXT));
    assert_eq!(w.sink.messages(), [CARRIED_TEXT]);
}

/// CH3b D238: as in
/// [`a_carried_stop_in_a_wait_under_a_release_point_is_not_refused_at_the_accept`], but the
/// operation reconsidered a1 first, so ABC has a step after b's release-point service. A lane
/// command in b's C5′ wait fails with the latch open, and V carries past it; b is admitted and
/// its tail deferred. ABC takes its step out of IBD, which queues the ChainLock tip task behind
/// b's tail. V runs the tail and drops the task, as Core's stopped scheduler never runs it
/// (`init.cpp:336`), and stops on the lane command's failure, reported once.
// Core: validation.cpp:3830-3833, :3852; init.cpp:336
#[test]
fn a_carried_stop_in_a_wait_under_a_release_point_lets_abc_take_its_step() {
    let w = WSide::new();
    let (mut rig, a1) = reconsiderable(&w, true);
    let (rb, ended) = waiting_under_a_release_point(&rig, &a1);
    rig.v.send(lane_fails()).expect("V waits");
    let (r, uncarried) = ended.recv_timeout(SOON).expect("the operation ends");
    assert_eq!(r, Ok(()), "ABC took its step");
    assert!(!uncarried, "the lane command's failure was carried past");
    let b = rb.recv().expect("b's accept and tail ran");
    assert!(b.accepted && b.new_block, "{b:?}");
    let exit = rig.stop();
    assert_eq!(exit.fatal.as_ref().map(|f| f.0.as_str()), Some(LANE_TEXT));
    assert_ne!(exit_tip(&exit), Some(rig.genesis.hash()), "a step after the stop moved the tip");
    assert_eq!(w.sink.messages(), [LANE_TEXT]);
}

/// [`Admitting`] whose first NONE site once `armed` fails as BL-535's guarded store does at a
/// block's accept: it reports through the node's sink and trips the latch before V hears of it
/// (L-3).
struct FailsAtNextAccept {
    site: Admitting,
    sink: Arc<RecordingAbort>,
    armed: Arc<AtomicBool>,
}

impl FlushSite for FailsAtNextAccept {
    fn at_site(&mut self, m: FlushStateMode, h: &mut FlushHost<'_>) -> Result<SiteDecision, Fatal> {
        if m == FlushStateMode::None && self.armed.swap(false, Ordering::SeqCst) {
            self.sink.abort_node(CARRIED_TEXT, None);
            self.site.0.0.trip(Failure {
                kind: ErrorKind::NoSpace,
                site: "flush",
                message: CARRIED_TEXT.into(),
            });
            return Err(Fatal::new(CARRIED_TEXT));
        }
        self.site.at_site(m, h)
    }

    fn after_step(&mut self, d: SiteDecision, h: &mut FlushHost<'_>) -> Result<(), Fatal> {
        self.site.after_step(d, h)
    }

    fn service(&mut self, h: &mut FlushHost<'_>) -> Result<(), Fatal> {
        self.site.service(h)
    }

    fn abort(&mut self, cause: AbortCause, h: &mut FlushHost<'_>) -> bool {
        self.site.abort(cause, h)
    }

    fn admit(&self) -> Result<(), Fatal> {
        self.site.admit()
    }
}

/// The failure of a lane command BL-678's carried-stop tests serve in b's C5′ wait: an
/// `AbortNode` V reports itself, the latch left open (a chain read's, such as `GetForBlock`'s).
const LANE_TEXT: &str = "GetForBlock: a chain read failed";

/// The lane command that fails with [`LANE_TEXT`].
fn lane_fails() -> ChainCommand {
    ChainCommand::TestUnderMain(Box::new(|_| Err(Fatal::new(LANE_TEXT))))
}

/// BL-678 (OD-CH3-13), M-1: every carried stop's latch read counts. As in
/// [`a_carried_stop_in_a_wait_under_a_release_point_is_not_refused_at_the_accept`], b waits in
/// its C5′ wait at the release point of an ABC with no step to take. A lane command there fails
/// with the latch open, and V carries past it with its report. The wait ends on V's stop, and b
/// is admitted (the latch is still open). b's accept then carries a second stop, its NONE flush's
/// store failure, which reports and trips the latch itself (BL-535's shape). That stop read the
/// latch tripped, so b's ABC tail runs as at the base: b's reply is the accepted block's, V's
/// stop is the first carried failure, and the node has both reports, in order.
// Core: validation.cpp:3852, :4865-4867, :4911-4916; shutdown.cpp:27-38
#[test]
fn a_later_carried_stop_that_trips_the_latch_lets_the_block_tail_run() {
    let w = WSide::new();
    let armed = Arc::new(AtomicBool::new(false));
    let mut rig = Rig::with(RigConfig {
        after_genesis: Some(3_600),
        flush: Some(Box::new(FailsAtNextAccept {
            site: Admitting(Latched(w.latch.clone())),
            sink: w.sink.clone(),
            armed: armed.clone(),
        })),
        abort: Some(w.sink.clone()),
        ..RigConfig::default()
    });
    let a1 = child(&rig.p, &rig.genesis, 0);
    rig.accept(&a1);
    rig.sync();
    let (rb, ended) = waiting_at_abcs_release_point(&rig, None);
    armed.store(true, Ordering::SeqCst);
    rig.v.send(lane_fails()).expect("V waits");
    let (r, uncarried) = ended.recv_timeout(SOON).expect("the operation ends");
    assert_eq!(r, Ok(()), "ABC has no step to take");
    assert!(!uncarried, "the lane command's failure was carried past");
    let b = rb.recv().expect("b's tail ran");
    assert!(b.accepted && b.new_block, "{b:?}");
    let exit = rig.stop();
    assert_eq!(exit.fatal.as_ref().map(|f| f.0.as_str()), Some(LANE_TEXT));
    assert_eq!(w.sink.messages(), [LANE_TEXT, CARRIED_TEXT]);
}

/// An [`AbortSink`] over `sink` that runs `then` once, after forwarding the report of
/// [`LANE_TEXT`].
struct ThenOnLaneReport {
    sink: Arc<RecordingAbort>,
    then: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl AbortSink for ThenOnLaneReport {
    fn abort_node(&self, log_msg: &str, user_msg: Option<&str>) {
        self.sink.abort_node(log_msg, user_msg);
        if log_msg == LANE_TEXT {
            let then = self.then.lock().expect("then").take();
            if let Some(then) = then {
                then();
            }
        }
    }

    fn read_failed(&self, what: &str) {
        self.sink.read_failed(what);
    }
}

/// BL-678 (OD-CH3-13), m-1: while every carried stop read the latch open, the accept after C5′'s
/// wait is admitted. b waits in its C5′ wait at the release point of an ABC with no step to
/// take; a lane command there fails with the latch open, and V carries past it. As V reports that
/// failure, after its stop read the latch open, W's store failure latches with its report held.
/// The wait ends on V's stop, and V waits at b's admission for W's report: b's accept never runs
/// (no header, no `BlockChecked`). The refusal is V's own, so ABC unwinds with W's text, b's reply
/// disconnects, and the node has both reports, in order.
// Core: shutdown.cpp:27-38; validation.cpp:4901-4905 at v24.0.0
#[test]
fn a_w_failure_after_a_latch_open_carried_stop_is_refused_at_the_accept() {
    let w = WSide::new();
    let (latch, held) = (w.latch.clone(), w.held.clone());
    let sink = Arc::new(ThenOnLaneReport {
        sink: w.sink.clone(),
        then: Mutex::new(Some(Box::new(move || drop(w_fails(&latch, &held, false))))),
    });
    let mut rig = Rig::with(RigConfig {
        after_genesis: Some(3_600),
        flush: Some(Box::new(Admitting(Latched(w.latch.clone())))),
        abort: Some(sink),
        ..RigConfig::default()
    });
    let a1 = child(&rig.p, &rig.genesis, 0);
    rig.accept(&a1);
    rig.sync();
    let v = rig.v_thread();
    let release = w.held.arm();
    let (rb, ended) = waiting_at_abcs_release_point(&rig, None);
    let b = child(&rig.p, &rig.genesis, 1).hash();
    rig.v.send(lane_fails()).expect("V waits");
    let parked = w.held.parked(&w.latch, v);
    let unknown = rig.v.reader().lookup(&b).is_none();
    let checked = rig.rec.take().iter().any(|e| matches!(e, Ev::Checked(h, _) if *h == b));
    let early = ended.try_recv().is_ok();
    release.release();
    assert!(parked.is_some(), "V waits for W's report at b's admission");
    assert!(unknown, "b's accept did not run before W's report");
    assert!(!checked, "no BlockChecked before W's report");
    assert!(!early, "the operation ended before W's report");
    let (r, _) = ended.recv_timeout(SOON).expect("the operation ends");
    assert_eq!(r, Err(W_STORE_TEXT.to_owned()));
    assert_eq!(rb.recv().map(|_| ()), Err(WaitError::Disconnected));
    assert!(rig.v.reader().lookup(&b).is_none(), "b's accept never ran");
    let checked = rig.rec.take().iter().any(|e| matches!(e, Ev::Checked(h, _) if *h == b));
    assert!(!checked, "no BlockChecked");
    let exit = rig.stop();
    assert_eq!(exit.fatal.as_ref().map(|f| f.0.as_str()), Some(W_STORE_TEXT));
    assert_eq!(w.sink.messages(), [LANE_TEXT, W_STORE_TEXT]);
}

/// The store failure [`a_carried_stop_in_a_wait_under_a_release_point_is_not_refused_at_the_accept`]
/// carries past.
const CARRIED_TEXT: &str = "Disk space is too low!";

/// BL-678 (C2): a refusal inside a wait nested under a release point's dispatch unwinds the
/// operation. Out of IBD, ABC's release point serves b's `ProcessBlock`, whose C5′ wait (a
/// closed accept gate) serves the lane; W's failure latches, then a query is queued and refused
/// there. The refusal is V's own, so the outer `carry_past` returns it: ABC takes no step after
/// its release point (a1 stays unconnected), nothing is carried past, both replies disconnect,
/// and the node has W's one report.
// Core: validation.cpp:3764-3771; shutdown.cpp:27-38
#[test]
fn a_refusal_in_a_wait_under_a_release_point_unwinds_the_operation() {
    let w = WSide::new();
    let (mut rig, a1) = reconsiderable(&w, true);
    let v = rig.v_thread();
    let release = w.held.arm();
    let (rb, ended) = waiting_under_a_release_point(&rig, &a1);
    drop(w.store_fails());
    let query = rig.v.query(|_| ());
    let parked = w.held.parked(&w.latch, v);
    let early = ended.try_recv().is_ok() || rig.shutdown.shutdown_requested();
    release.release();
    assert!(parked.is_some(), "V waits for W's report at the wait's admission");
    assert!(!early, "the operation ended before W's report");
    let (r, uncarried) = ended.recv_timeout(SOON).expect("the operation ends");
    assert_eq!(r, Err(W_STORE_TEXT.to_owned()));
    assert!(uncarried, "the refusal was not carried past");
    assert_eq!(rb.recv().map(|_| ()), Err(WaitError::Disconnected));
    assert_eq!(query.recv(), Err(WaitError::Disconnected));
    let exit = rig.stop();
    assert_eq!(exit.fatal.as_ref().map(|f| f.0.as_str()), Some(W_STORE_TEXT));
    assert_eq!(exit_tip(&exit), Some(rig.genesis.hash()));
    assert_eq!(w.sink.messages(), [W_STORE_TEXT]);
}

/// A light flush site that takes V's bell and answers it, and V's admissions, with the latch's
/// waiting read (BL-678's optional (b)).
struct Belled {
    latch: GlobalFailStop,
    bell: Arc<Mutex<Option<FlushBell>>>,
}

impl FlushSite for Belled {
    fn attach(&mut self, bell: FlushBell) {
        *self.bell.lock().expect("bell") = Some(bell);
    }

    fn at_site(&mut self, m: FlushStateMode, h: &mut FlushHost<'_>) -> Result<SiteDecision, Fatal> {
        Latched(self.latch.clone()).at_site(m, h)
    }

    fn after_step(&mut self, d: SiteDecision, h: &mut FlushHost<'_>) -> Result<(), Fatal> {
        Latched(self.latch.clone()).after_step(d, h)
    }

    fn service(&mut self, _: &mut FlushHost<'_>) -> Result<(), Fatal> {
        admitted(&self.latch)
    }

    fn answer_due(&mut self, _: &mut FlushHost<'_>) -> Result<(), Fatal> {
        admitted(&self.latch)
    }

    fn abort(&mut self, cause: AbortCause, h: &mut FlushHost<'_>) -> bool {
        Latched(self.latch.clone()).abort(cause, h)
    }

    fn admit(&self) -> Result<(), Fatal> {
        admitted(&self.latch)
    }
}

/// BL-678, optional (b): W's failure taken at a nested wait's bell is V's own. As in
/// [`a_refusal_in_a_wait_under_a_release_point_unwinds_the_operation`], ABC's release point serves
/// b's `ProcessBlock`, whose C5′ wait (the accept gate stays closed) is open when W's failure
/// latches; here W rings V's bell and no command is queued. The wait answers the bell and reads
/// W's failure there, waiting for W's report; the outer `carry_past` then returns it: ABC takes no
/// step after its release point (a1 stays unconnected), nothing is carried past, b's reply
/// disconnects, and the node has W's one report.
// Core: validation.cpp:3764-3771; shutdown.cpp:27-38
#[test]
fn ws_failure_at_a_nested_waits_bell_unwinds_the_operation() {
    let w = WSide::new();
    let bell = Arc::new(Mutex::new(None));
    let mut rig = Rig::with(RigConfig {
        after_genesis: Some(3_600),
        flush: Some(Box::new(Belled {
            latch: w.latch.clone(),
            bell: bell.clone(),
        })),
        abort: Some(w.sink.clone()),
        ..RigConfig::default()
    });
    let a1 = invalidated_a1(&rig);
    let v = rig.v_thread();
    let release = w.held.arm();
    let (rb, ended) = waiting_under_a_release_point(&rig, &a1);
    drop(w.store_fails());
    bell.lock().expect("bell").as_ref().expect("V attached its bell").ring();
    let parked = w.held.parked(&w.latch, v);
    let early = ended.try_recv().is_ok() || rig.shutdown.shutdown_requested();
    release.release();
    assert!(parked.is_some(), "V waits for W's report at the wait's bell");
    assert!(!early, "the operation ended before W's report");
    let (r, uncarried) = ended.recv_timeout(SOON).expect("the operation ends");
    assert_eq!(r, Err(W_STORE_TEXT.to_owned()));
    assert!(uncarried, "W's failure at the bell was not carried past");
    assert_eq!(rb.recv().map(|_| ()), Err(WaitError::Disconnected));
    let exit = rig.stop();
    assert_eq!(exit.fatal.as_ref().map(|f| f.0.as_str()), Some(W_STORE_TEXT));
    assert_eq!(exit_tip(&exit), Some(rig.genesis.hash()));
    assert_eq!(w.sink.messages(), [W_STORE_TEXT]);
}

/// BL-678, path 6: a `ProcessBlock` that waited in C5′'s wait is admitted again before its
/// accept. Idle V, out of IBD, takes a's block, whose wait (a closed accept gate) is open while
/// W's failure latches; the gate then opens and the wait ends on its condition. V waits for
/// W's report there, and the accept never runs: no header, no `NewPoWValidBlock`, no
/// `BlockChecked`, and the reply disconnects.
// Core: validation.cpp:4901-4905 at v24.0.0; shutdown.cpp:27-38
#[test]
fn an_accept_after_its_wait_is_admitted_again() {
    let w = WSide::new();
    let mut rig = Rig::with(w.config(RigConfig {
        after_genesis: Some(3_600),
        ..RigConfig::default()
    }));
    let gates = Arc::new(Gates::default());
    rig.signals.register(gates.clone());
    let a = child(&rig.p, &rig.genesis, 0);
    let v = rig.v_thread();
    gates.accept.close();
    let release = w.held.arm();
    let ra = rig.v.process_block(a.block.clone(), true, None, true);
    assert_eq!(gates.entered(), "accept");
    drop(w.store_fails());
    gates.accept.open();
    let parked = w.held.parked(&w.latch, v);
    let early = (ra.try_recv().is_some(), rig.v.reader().lookup(&a.hash()).is_some());
    release.release();
    assert!(parked.is_some(), "V waits for W's report after the wait");
    assert_eq!(early, (false, false), "the accept ran before W's report");
    assert_eq!(ra.recv().map(|_| ()), Err(WaitError::Disconnected));
    let exit = rig.stop();
    assert_eq!(exit.fatal.as_ref().map(|f| f.0.as_str()), Some(W_STORE_TEXT));
    assert!(rig.v.reader().lookup(&a.hash()).is_none(), "no header accepted");
    let seen = rig.rec.take();
    assert!(
        !seen.iter().any(|e| matches!(e, Ev::Checked(..) | Ev::PowValid(_) | Ev::Header(_))),
        "{seen:?}"
    );
    assert_eq!(w.sink.messages(), [W_STORE_TEXT]);
}

/// D240 (E4-B) at the ABC command's own first `LimitValidationInterfaceQueue` (`v:3771`), before
/// any search or step: inside that wait (the signals thread held past Core's limit) the lane
/// command the wait's select takes after W's failure has latched is refused once W has
/// reported, and the refusal ends the ABC at its first boundary, so the command answers `Ok`,
/// after W's report and the node's flag. a1 is invalidated, so there is no work; the command's
/// reply disconnects.
// Core: validation.cpp:3771, :3789-3791, :3823; node/blockstorage.cpp:952-953
#[test]
fn an_abc_command_meeting_ws_failure_in_its_first_queue_wait_answers_ok() {
    let w = WSide::new();
    let (mut rig, a1) = reconsiderable(&w, false);
    let v = rig.v_thread();
    let (backlog, hold) = Backlog::register(&rig);
    backlog.hold(&rig);
    let release = w.held.arm();
    let abc = rig.v.activate_best_chain();
    backlog.until_v_waits();
    drop(w.store_fails());
    let queued = rig.v.process_headers(batch(std::slice::from_ref(&a1)), None);
    let parked = w.held.parked(&w.latch, v);
    let early = abc.try_recv().is_some() || rig.shutdown.shutdown_requested();
    let abc = at_close(abc, &rig.shutdown, rig.v.loading(), &w.sink);
    release.release();
    assert!(parked.is_some(), "V waits for W's report at the admission");
    assert!(!early, "the ABC ended before W's report");
    assert_eq!(abc(), (Ok(Ok(())), Some((true, false, 1))));
    assert_eq!(w.sink.messages(), [W_STORE_TEXT]);
    assert_eq!(queued.recv().map(|_| ()), Err(WaitError::Disconnected));
    drop(hold);
    let exit = rig.stop();
    backlog.unregister(&rig);
    assert_eq!(exit.fatal.as_ref().map(|f| f.0.as_str()), Some(W_STORE_TEXT));
    assert_eq!(exit_tip(&exit), Some(rig.genesis.hash()));
}
