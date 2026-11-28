//! O15-3: the ChainLock divergence tripwire on V (spec §3, tests §5 (a)–(t)), in the locks
//! harness. The tripwire's monotonic clock is the test's `AtomicU64`; the node clock is the rig's
//! mock, placed so the tip is out of IBD unless a test says otherwise. Each block is a second
//! after its parent, so long chains stay inside Core's two-hour future limit.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use dash_chain::Status;
use dash_consensus::{Block, BlockId, BlockValidationError, BlockValidationResult};
use dash_primitives::BlockHash;

use super::super::support::{
    Mined, Rig, RigConfig, SharedFiles, cb_script, coinbase, install_accept_log, lines_with, mine,
};
use super::{ClLog, cl, cl_rig, floor, intake, lock, lock_at, record, settle};
use crate::chain::actor::V;
use crate::chain::tripwire::{
    HookScope, REJECTED_CAPACITY, TRIPWIRE_DEADLINE, TRIPWIRE_DEEP_ADMIT, TRIPWIRE_NEAR_BLOCKS,
    latching_ibd_in_hook,
};
use crate::chain::{
    BlockConnector, BlockResult, BlockState, BlockStatusWriter, ChainLockSig, ConnectCx,
    ConnectOutcome, DisconnectResult, DivergenceEvent, DivergenceInfo, DivergenceKind, Fatal,
    NullConnector, ProposalChecks, RejectionStage, Tripwire, VContext, ViewTarget,
};

const HOUR: u64 = TRIPWIRE_DEADLINE;
const NO_CONNECT: &str = "no block on the ChainLocked path connected within 60 min";

/// A child of `parent` a second later.
fn kid(rig: &Rig, parent: &Mined, salt: u32) -> Mined {
    let height = parent.height + 1;
    let cb = coinbase(&cb_script(height, salt));
    Mined {
        block: mine(&rig.p, parent.hash(), parent.time() + 1, vec![cb]),
        height,
    }
}

/// `n` children in a line on `parent`.
fn line(rig: &Rig, parent: &Mined, n: usize, salt: u32) -> Vec<Mined> {
    let mut out: Vec<Mined> = Vec::with_capacity(n);
    for _ in 0..n {
        let next = kid(rig, out.last().unwrap_or(parent), salt);
        out.push(next);
    }
    out
}

/// A child of `parent` with a second coinbase: context-free CheckBlock's `bad-cb-multiple`.
fn bad_coinbase(rig: &Rig, parent: &Mined, salt: u32) -> Mined {
    let height = parent.height + 1;
    let txs = vec![coinbase(&cb_script(height, salt)), coinbase(&cb_script(height, salt + 1))];
    Mined {
        block: mine(&rig.p, parent.hash(), parent.time() + 1, txs),
        height,
    }
}

fn reject_reason(r: &BlockResult) -> String {
    match &r.state {
        BlockState::Invalid(e) => e.reject_reason().to_owned(),
        s => format!("{s:?}"),
    }
}

/// The blocks whose ConnectBlock fails, once or every time, with the planted reason, and those
/// whose ConnectBlock ends in an error state.
#[derive(Clone, Default)]
struct Fails(
    Arc<Mutex<HashMap<BlockHash, (bool, BlockValidationResult)>>>,
    Arc<Mutex<HashMap<BlockHash, String>>>,
);

impl Fails {
    fn set(&self, m: &Mined, once: bool, result: BlockValidationResult) {
        self.0.lock().expect("fails").insert(m.hash(), (once, result));
    }

    fn always(&self, m: &Mined) {
        self.set(m, false, BlockValidationResult::Consensus);
    }

    fn once(&self, m: &Mined) {
        self.set(m, true, BlockValidationResult::Consensus);
    }

    fn clear(&self, m: &Mined) {
        self.0.lock().expect("fails").remove(&m.hash());
    }

    fn error(&self, m: &Mined, state: &str) {
        self.clear(m);
        self.1.lock().expect("errors").insert(m.hash(), state.to_owned());
    }
}

impl BlockConnector for Fails {
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
        if let Some(state) = self.1.lock().expect("errors").get(&block.hash()) {
            return Ok(ConnectOutcome::Unavailable(state.clone()));
        }
        let mut fails = self.0.lock().expect("fails");
        if let Some(&(once, result)) = fails.get(&block.hash()) {
            if once {
                fails.remove(&block.hash());
            }
            let err = BlockValidationError::new(result, "bad-planted");
            return Ok(ConnectOutcome::Invalid(err));
        }
        drop(fails);
        NullConnector.connect_block(cx, v, target, block, checked, id, st)
    }

    fn disconnect_block(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        target: ViewTarget<'_>,
        b: &Block,
        id: BlockId,
    ) -> Result<DisconnectResult, Fatal> {
        NullConnector.disconnect_block(cx, v, target, b, id)
    }

    fn test_block_validity(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        checks: &dyn ProposalChecks,
        b: &Block,
        prev: BlockId,
        pow: bool,
        merkle: bool,
    ) -> Result<ConnectOutcome, Fatal> {
        NullConnector.test_block_validity(cx, v, checks, b, prev, pow, merkle)
    }
}

/// A rig with the tripwire on the test's monotonic clock, SPORK_19 observed on, the tip out of
/// IBD, and a connector whose failures the test sets.
struct T {
    rig: Rig,
    log: Arc<ClLog>,
    mono: Arc<AtomicU64>,
    fails: Fails,
}

impl T {
    fn new() -> T {
        T::with(RigConfig::default(), true)
    }

    /// `cfg` with the connector and the UI set here; a disarmed V when `!armed`.
    fn with(cfg: RigConfig, armed: bool) -> T {
        let fails = Fails::default();
        let (rig, log) = cl_rig(RigConfig {
            after_genesis: cfg.after_genesis.or(Some(3_600)),
            connector: Some(Box::new(fails.clone())),
            ..cfg
        });
        let mono = Arc::new(AtomicU64::new(0));
        let m = mono.clone();
        rig.on_v(move |v| {
            if armed {
                // Explicitly, so a run under `DASHD_TEST_TRIPWIRE_DISABLED` still arms these.
                v.tripwire = Tripwire::armed();
                v.tripwire.set_mono(m);
            } else {
                v.tripwire = Tripwire::disabled_tripwire();
            }
            Ok(())
        });
        intake(&rig);
        T {
            rig,
            log,
            mono,
            fails,
        }
    }

    fn at(&self, secs: u64) {
        self.mono.store(secs, Ordering::Release);
    }

    /// The 5 s timer's enforcement at `secs`.
    fn tick(&self, secs: u64) {
        self.at(secs);
        self.rig.v.chainlock_timer().expect("V runs");
        settle(&self.rig);
    }

    /// Ticks every `step` seconds over `(from, to]`.
    fn ticks(&self, from: u64, to: u64, step: u64) {
        let mut now = from;
        while now < to {
            now = (now + step).min(to);
            self.tick(now);
        }
    }

    /// Records a lock on `m` at its height at `secs`, and runs what that queued.
    fn lock(&self, secs: u64, m: &Mined) -> bool {
        self.lock_sig(secs, &lock(m))
    }

    fn lock_sig(&self, secs: u64, sig: &Arc<ChainLockSig>) -> bool {
        self.at(secs);
        let relay = record(&self.rig, sig);
        settle(&self.rig);
        relay
    }

    fn accept(&self, secs: u64, m: &Mined) {
        self.at(secs);
        self.rig.accept(m);
        settle(&self.rig);
    }

    fn submit(&self, secs: u64, m: &Mined) -> BlockResult {
        self.at(secs);
        let r = self.rig.submit(m);
        settle(&self.rig);
        r
    }

    fn events(&self) -> Vec<DivergenceEvent> {
        std::mem::take(&mut *self.log.divergence.lock().expect("divergence"))
    }

    fn with_v<R: Send + 'static>(&self, f: impl FnOnce(&mut V) -> R + Send + 'static) -> R {
        let (tx, rx) = mpsc::channel();
        self.rig.on_v(move |v| {
            tx.send(f(v)).expect("the test waits");
            Ok(())
        });
        rx.recv().expect("V ran it")
    }

    fn tw<R: Send + 'static>(&self, f: impl FnOnce(&Tripwire) -> R + Send + 'static) -> R {
        self.with_v(move |v| f(&v.tripwire))
    }

    fn onset_since(&self) -> Option<u64> {
        self.tw(|w| w.onset().map(|o| o.since))
    }

    fn t1_episode(&self) -> Option<String> {
        self.tw(|w| w.t1_episode().map(str::to_owned))
    }

    fn latch(&self) -> Option<DivergenceInfo> {
        self.tw(|w| w.tripped().cloned())
    }

    fn rejection(&self, m: &Mined) -> Option<RejectionStage> {
        let id = self.rig.id(m);
        self.tw(move |w| w.rejected().get(id).map(|r| r.stage))
    }

    fn active(&self, m: &Mined) -> bool {
        let id = self.rig.id(m);
        self.with_v(move |v| v.chain.active_contains_private(id))
    }

    fn restore(&self, info: DivergenceInfo) {
        self.rig.on_v(move |v| {
            v.tripwire.restore(info);
            Ok(())
        });
    }

    /// The ids of the latching `is_ibd` reads made inside a tripwire hook so far.
    fn latching_reads(&self) -> u64 {
        self.with_v(|_| latching_ibd_in_hook())
    }
}

fn first(events: &[DivergenceEvent]) -> &DivergenceInfo {
    match events {
        [DivergenceEvent::First(info)] => info,
        other => panic!("one first event: {other:?}"),
    }
}

fn info_of(e: &DivergenceEvent) -> &DivergenceInfo {
    match e {
        DivergenceEvent::First(i)
        | DivergenceEvent::Realert(i)
        | DivergenceEvent::Redelivered(i) => i,
    }
}

fn firsts(events: &[DivergenceEvent]) -> usize {
    events.iter().filter(|e| matches!(e, DivergenceEvent::First(_))).count()
}

/// A tip at height 2 (`main[1]`) with `main[2]` failing ConnectBlock every time.
fn connect_failure(t: &T, salt: u32) -> Vec<Mined> {
    let main = line(&t.rig, &t.rig.genesis, 5, salt);
    t.accept(0, &main[0]);
    t.accept(0, &main[1]);
    t.fails.always(&main[2]);
    t.submit(0, &main[2]);
    assert!(t.rig.status(&main[2]).has(Status::FAILED_VALID));
    main
}

/// (a) A ConnectBlock rejection, then a lock on that block: the reset retries it, ABC fails it
/// again, the floor stays off the active chain, and T1 trips with the reason and stage.
#[test]
fn a_connect_stage_rejection_under_a_lock_trips_t1() {
    let t = T::new();
    let main = connect_failure(&t, 15_001);
    let x = &main[2];
    assert_eq!(t.rejection(x), Some(RejectionStage::Connect));
    assert!(t.events().is_empty());
    assert!(t.lock(10, x));
    let events = t.events();
    let info = first(&events);
    assert_eq!(info.kind, DivergenceKind::T1);
    assert_eq!(info.stage, Some(RejectionStage::Connect));
    assert!(info.reason.starts_with("stage: Connect, bad-planted"), "{}", info.reason);
    assert_eq!((info.hash, info.height), (x.hash(), x.height));
    assert_eq!((info.cl_hash, info.floor_hash, info.onset_cl), (x.hash(), Some(x.hash()), None));
    assert_eq!(info.h_obs, 2);
    assert!(info.episode.starts_with("T1-3-"), "{}", info.episode);
    assert_eq!(t.latch().as_ref(), Some(info));
    assert_eq!(t.latching_reads(), 0);
}

/// A ConnectBlock rejection, then a lock on that block whose reset retries it into an error
/// state: the enforcement logs ABC's failure and returns, before T1.
// Core: chainlock/handler.cpp:307-310 at v24.0.3
#[test]
fn an_abc_error_on_the_locks_retry_ends_the_enforcement_before_t1() {
    let t = T::new();
    let main = connect_failure(&t, 15_901);
    let x = &main[2];
    assert_eq!(t.rejection(x), Some(RejectionStage::Connect));
    t.fails.error(x, "a failed connect");
    assert!(t.lock(10, x));
    assert!(t.events().is_empty(), "{:?}", t.events());
    assert_eq!(super::super::failure::tip(&t.rig), t.rig.id(&main[1]), "X is not connected");
}

/// (b) An Accept-stage plant (no data stored), then a lock: T1 after the reset, as ABC has no
/// data to retry. Then the reverse order: the lock first, then the rejection, caught by the next
/// timer enforcement.
#[cfg(feature = "planted-bugs")]
#[test]
fn b_an_accept_stage_plant_trips_t1_in_either_order() {
    use crate::Planted;

    let t = T::new();
    let main = line(&t.rig, &t.rig.genesis, 3, 15_011);
    t.accept(0, &main[0]);
    t.accept(0, &main[1]);
    let x = &main[2];
    t.rig.v.plant(Some(Planted::RejectAcceptAtHeight(x.height)));
    let r = t.submit(0, x);
    assert_eq!(reject_reason(&r), "bad-planted");
    assert!(t.rig.status(x).has(Status::FAILED_VALID));
    assert!(!t.rig.status(x).has(Status::HAVE_DATA));
    assert_eq!(t.rejection(x), Some(RejectionStage::Accept));
    assert!(t.lock(10, x));
    let events = t.events();
    let info = first(&events);
    assert_eq!((info.kind, info.stage), (DivergenceKind::T1, Some(RejectionStage::Accept)));
    assert!(info.reason.starts_with("stage: Accept, bad-planted"), "{}", info.reason);

    let t = T::new();
    let main = line(&t.rig, &t.rig.genesis, 3, 15_012);
    t.accept(0, &main[0]);
    t.accept(0, &main[1]);
    let x = &main[2];
    t.rig.headers(std::slice::from_ref(x));
    assert!(t.lock(10, x));
    assert_eq!(floor(&t.rig), Some(t.rig.id(x)));
    t.rig.v.plant(Some(Planted::RejectAcceptAtHeight(x.height)));
    assert_eq!(reject_reason(&t.submit(20, x)), "bad-planted");
    assert!(t.events().is_empty(), "no enforcement has run since");
    t.tick(25);
    let events = t.events();
    let info = first(&events);
    assert_eq!((info.kind, info.stage), (DivergenceKind::T1, Some(RejectionStage::Accept)));
}

/// (b2) A P2P block failing context-free CheckBlock with its header indexed: tripwire evidence
/// only (no status change, `failed_blocks` unchanged, the reply a disarmed V gives). A lock on
/// it with no honest body is no T1, and T2 trips at 60 min with `stage: Check` in its reason. A
/// block whose header is not indexed inserts nothing.
#[test]
fn b2_a_check_stage_rejection_alone_is_no_t1_and_names_t2s_stage() {
    let run = |armed: bool| {
        let t = T::with(RigConfig::default(), armed);
        let a1 = kid(&t.rig, &t.rig.genesis, 15_021);
        t.accept(0, &a1);
        let x = bad_coinbase(&t.rig, &a1, 15_022);
        t.rig.headers(std::slice::from_ref(&x));
        let before = t.rig.status(&x);
        let failed = failed_blocks(&t);
        let r = t.submit(0, &x);
        assert_eq!(reject_reason(&r), "bad-cb-multiple");
        assert_eq!(t.rig.status(&x), before);
        assert_eq!(failed_blocks(&t), failed);
        let unindexed = bad_coinbase(&t.rig, &a1, 15_024);
        let unindexed_reply = t.submit(0, &unindexed);
        (t, x, format!("{r:?}"), format!("{unindexed_reply:?}"))
    };
    let (_, _, disarmed, disarmed_unindexed) = run(false);
    let (t, x, armed, armed_unindexed) = run(true);
    assert_eq!((armed, armed_unindexed), (disarmed, disarmed_unindexed));
    assert_eq!(t.rejection(&x), Some(RejectionStage::Check));
    assert_eq!(t.tw(|w| w.rejected().len()), 1, "the unindexed block inserted nothing");

    assert!(t.lock(10, &x));
    assert!(t.events().is_empty(), "a Check-stage entry is no T1");
    assert_eq!(t.onset_since(), Some(10));
    t.ticks(10, 10 + HOUR - 1, 300);
    assert!(t.events().is_empty());
    t.tick(10 + HOUR);
    let events = t.events();
    let info = first(&events);
    assert_eq!((info.kind, info.stage), (DivergenceKind::T2, Some(RejectionStage::Check)));
    assert!(info.reason.starts_with("stage: Check, bad-cb-multiple"), "{}", info.reason);
    assert_eq!(info.onset_cl, Some(x.hash()));
}

fn failed_blocks(t: &T) -> Vec<BlockId> {
    t.with_v(|v| {
        let mut ix = v.chain.node_index().expect("the index");
        ix.failed_blocks().iter().collect()
    })
}

/// (c) A lock on X+1 after X was rejected: its header is refused, so the lock is Pending, and T2
/// trips at 60 min.
#[test]
fn c_a_lock_past_a_rejected_block_trips_t2_at_an_hour() {
    let t = T::new();
    let main = connect_failure(&t, 15_031);
    let x1 = &main[3];
    let r = t.rig.v.process_headers(super::batch(std::slice::from_ref(x1)), None).recv();
    assert!(r.expect("V replies").stop.is_some(), "a failed parent refuses the header");
    assert!(t.lock(100, x1));
    t.tick(100);
    assert_eq!(t.onset_since(), Some(100));
    t.ticks(100, 100 + HOUR - 1, 60);
    assert!(t.events().is_empty());
    t.tick(100 + HOUR);
    let events = t.events();
    let info = first(&events);
    assert_eq!((info.kind, info.stage), (DivergenceKind::T2, None));
    assert_eq!(info.reason, NO_CONNECT);
    assert_eq!((info.cl_hash, info.onset_cl, info.floor_hash), (x1.hash(), Some(x1.hash()), None));
}

/// A tip at height 1 and `n` network blocks above it the node never sees.
fn stalled(t: &T, n: usize, salt: u32) -> Vec<Mined> {
    let a1 = kid(&t.rig, &t.rig.genesis, salt);
    t.accept(0, &a1);
    line(&t.rig, &a1, n, salt + 1)
}

/// (d), (d2) A stall at h−1 with no sibling and a new lock every `every` seconds: no trip at
/// 59 min, a trip at 60 min after the original onset, whether or not the locks pass h+48.
fn stall_with_locks_every(every: u64, salt: u32) {
    let t = T::new();
    let net = stalled(&t, 64, salt);
    let mut now = 0;
    let mut i = 0;
    while now < HOUR {
        assert!(t.lock(now, &net[i]));
        t.tick(now);
        assert_eq!(t.onset_since(), Some(0), "the onset is kept at {now} s");
        assert!(t.events().is_empty(), "{now}");
        i += 1;
        now += every;
    }
    t.tick(HOUR - 1);
    assert!(t.events().is_empty());
    t.tick(HOUR);
    let events = t.events();
    let info = first(&events);
    assert_eq!(info.kind, DivergenceKind::T2);
    assert_eq!(info.onset_cl, Some(net[0].hash()));
    assert_eq!(info.cl_hash, net[i - 1].hash());
    assert_eq!(info.h_obs, 1);
}

#[test]
fn d_a_stall_with_a_lock_every_150_s_trips_at_60_min() {
    stall_with_locks_every(150, 15_041);
}

#[test]
fn d2_faster_locks_past_n_keep_the_onset() {
    // The best lock passes h+48 at about 56 min.
    stall_with_locks_every(70, 15_043);
}

/// (d3) A node 100 blocks behind with C′ holding and a frozen tip: no onset at 599 s, an onset
/// backdated to the first observation at 600 s, and a trip 3,600 s after that observation.
#[test]
fn d3_a_deep_frozen_stall_admits_at_600_s_backdated() {
    let t = T::new();
    let net = stalled(&t, 100, 15_051);
    let t0 = 1_000;
    assert!(t.lock(t0, &net[99]));
    t.tick(t0);
    assert_eq!(t.tw(|w| w.deep_since()), Some(t0));
    t.ticks(t0, t0 + TRIPWIRE_DEEP_ADMIT - 1, 60);
    assert_eq!(t.onset_since(), None);
    t.tick(t0 + TRIPWIRE_DEEP_ADMIT);
    assert_eq!(t.onset_since(), Some(t0), "backdated to the first observation");
    t.ticks(t0 + TRIPWIRE_DEEP_ADMIT, t0 + HOUR - 1, 300);
    assert!(t.events().is_empty());
    t.tick(t0 + HOUR);
    let events = t.events();
    let info = first(&events);
    assert_eq!(
        (info.kind, info.onset_cl, info.h_obs),
        (DivergenceKind::T2, Some(net[99].hash()), 1)
    );
}

/// A catch-up `behind` blocks back: one block connected a minute, the network mining a block every
/// 150 s with a lock on it, for four hours. Returns the onsets seen, each with its lock block.
fn catch_up(behind: usize, salt: u32) -> Vec<(u64, BlockHash)> {
    let t = T::new();
    let net = line(&t.rig, &t.rig.genesis, behind + 100, salt);
    let mut have = 0;
    let mut mined = behind;
    assert!(t.lock(0, &net[mined - 1]));
    t.tick(0);
    let mut onsets: Vec<(u64, BlockHash)> = Vec::new();
    for step in 1..=240u64 {
        let now = 60 * step;
        if have < mined {
            t.accept(now, &net[have]);
            have += 1;
        }
        while mined < behind + (now / 150) as usize {
            mined += 1;
            t.lock(now, &net[mined - 1]);
        }
        t.tick(now);
        let gap = mined - have;
        let onset = t.tw(|w| w.onset().map(|o| (o.since, o.cl)));
        // An A1 onset admitted at a gap of N within this step may see one more lock after it.
        if gap > TRIPWIRE_NEAR_BLOCKS as usize + 1 {
            assert_eq!(onset, None, "deep keeps dropping at {now} s, gap {gap}");
            assert!(t.tw(|w| w.deep_since()).is_some_and(|since| now - since < 600));
        }
        if let Some((since, cl)) = onset {
            match onsets.last() {
                Some(&(s, c)) if s == since => assert_eq!(c, cl, "onset_cl is frozen"),
                last => {
                    if let Some(&(_, c)) = last {
                        assert!(
                            net.iter().position(|m| m.hash() == c).is_some_and(|i| i < have),
                            "the onset cleared when its lock's block connected"
                        );
                    }
                    onsets.push((since, cl));
                }
            }
            assert!(now - since < HOUR, "no onset ages to the deadline: {now} s");
        }
        assert!(t.events().is_empty(), "{now} s");
    }
    assert_eq!(t.latch(), None);
    onsets
}

/// (d3, negative) Deep catch-up with progress: `deep` keeps dropping while the gap is above N;
/// once within N, each A1 onset clears when its lock's block connects and re-admits. No trip
/// and no latch through 4 h.
#[test]
fn d3_a_deep_catch_up_with_progress_never_trips() {
    let onsets = catch_up(100, 15_053);
    assert!(onsets.len() > 1, "{onsets:?}");
}

/// (d4) An A1 catch-up 40 blocks back: an onset at the first observation, cleared when its lock's
/// block connects, re-admitted against the current lock. No trip, no latch through 4 h.
#[test]
fn d4_a_near_catch_up_clears_and_readmits() {
    let onsets = catch_up(40, 15_061);
    assert_eq!(onsets[0].0, 0, "admitted at the first observation");
    assert!(onsets.len() > 2, "{onsets:?}");
}

/// (e) A Pending lock, its header late (Pending → known), and the block at 40 min: the onset
/// clears and nothing trips.
#[test]
fn e_a_late_header_then_the_block_resolves() {
    let t = T::new();
    let net = stalled(&t, 1, 15_071);
    let m = &net[0];
    assert!(t.lock(0, m));
    t.tick(0);
    assert_eq!(t.onset_since(), Some(0));
    t.at(1_200);
    t.rig.headers(std::slice::from_ref(m));
    t.tick(1_200);
    assert_eq!(cl(&t.rig).best_known.as_ref().map(|s| s.block_hash), Some(m.hash()));
    assert_eq!(t.onset_since(), Some(0));
    t.accept(2_400, m);
    t.tick(2_400);
    assert_eq!(t.onset_since(), None);
    t.ticks(2_400, 4 * HOUR, 600);
    assert!(t.events().is_empty());
    assert_eq!(t.latch(), None);
}

/// (e2) The lock's header known and its data never arriving: an onset at the first observation,
/// no trip at 59 min, a trip at 60.
#[test]
fn e2_a_known_header_whose_data_never_arrives_trips() {
    let t = T::new();
    let net = stalled(&t, 1, 15_081);
    t.rig.headers(&net);
    assert!(t.lock(0, &net[0]));
    assert_eq!(t.onset_since(), Some(0), "the recorded enforcement observed it");
    t.ticks(0, HOUR - 1, 300);
    assert!(t.events().is_empty());
    t.tick(HOUR);
    let events = t.events();
    let info = first(&events);
    assert_eq!((info.kind, info.floor_hash), (DivergenceKind::T2, Some(net[0].hash())));
}

/// (e3) A Pending lock whose header arrives without data at 59 min, and enforcement then
/// disconnects our branch: the onset, H_obs and episode key are unchanged, and the trip comes at
/// 60 min.
#[test]
fn e3_pending_to_known_at_59_min_keeps_the_onset() {
    let t = T::new();
    let f = kid(&t.rig, &t.rig.genesis, 15_091);
    t.accept(0, &f);
    let ours = line(&t.rig, &f, 2, 15_092);
    for m in &ours {
        t.accept(0, m);
    }
    let net = line(&t.rig, &f, 3, 15_093);
    assert!(t.lock(0, &net[2]));
    t.tick(0);
    let onset = t.tw(|w| w.onset()).expect("an onset");
    assert_eq!((onset.since, onset.h_obs), (0, 3));
    assert_eq!(onset.key, (3, ours[1].hash()), "our tip: the lock's header is unknown");
    t.ticks(0, 3_540, 300);
    t.at(3_540);
    t.rig.headers(&net);
    t.tick(3_540);
    assert!(t.rig.status(&ours[0]).has(Status::CONFLICT_CHAINLOCK));
    assert!(t.active(&f) && !t.active(&ours[0]), "enforcement disconnected our branch");
    assert_eq!(t.tw(|w| w.onset()), Some(onset));
    t.tick(HOUR - 1);
    assert!(t.events().is_empty());
    t.tick(HOUR);
    let events = t.events();
    let info = first(&events);
    assert_eq!(info.h_obs, 3);
    assert_eq!(info.episode, format!("T2-3-{}", &ours[1].hash().to_string()[..12]));
}

/// (f) A lock above our tip whose block arrives at 5 min: clean.
#[test]
fn f_a_lock_whose_block_arrives_at_5_min_is_clean() {
    let t = T::new();
    let net = stalled(&t, 1, 15_101);
    assert!(t.lock(0, &net[0]));
    t.tick(0);
    t.accept(300, &net[0]);
    t.ticks(300, 2 * HOUR, 300);
    assert_eq!(t.onset_since(), None);
    assert!(t.events().is_empty());
    assert_eq!(t.latch(), None);
}

/// (g) A lock that reorgs our tip onto a valid sibling: clean.
#[test]
fn g_a_reorg_onto_a_valid_sibling_is_clean() {
    let t = T::new();
    let main = line(&t.rig, &t.rig.genesis, 3, 15_111);
    let side = line(&t.rig, &t.rig.genesis, 2, 15_112);
    for m in main.iter().chain(&side) {
        t.accept(0, m);
    }
    assert!(t.lock(0, &side[1]));
    assert!(t.active(&side[1]));
    t.ticks(0, 2 * HOUR, 300);
    assert_eq!(t.onset_since(), None);
    assert!(t.events().is_empty());
}

/// (h) SPORK_19 off for 90 min while C′ would otherwise hold, then on: the onset starts at the
/// flip (the lock redelivered then), and nothing trips before an hour after it.
#[test]
fn h_spork_off_time_never_ages_into_a_trip() {
    let t = T::new();
    let net = stalled(&t, 1, 15_121);
    assert!(t.lock(0, &net[0]));
    let sporks = t.rig.sporks.as_ref().expect("a fresh rig");
    sporks.set(false);
    intake(&t.rig);
    for now in (0..=5_400).step_by(600) {
        t.tick(now);
        assert_eq!(t.onset_since(), None, "{now}");
    }
    assert!(cl(&t.rig).best.is_some(), "the best lock is kept while off");
    sporks.set(true);
    intake(&t.rig);
    assert!(t.lock(5_400, &net[0]));
    t.tick(5_400);
    assert_eq!(t.onset_since(), Some(5_400));
    t.tick(5_400 + HOUR - 1);
    assert!(t.events().is_empty());
    t.tick(5_400 + HOUR);
    assert_eq!(first(&t.events()).kind, DivergenceKind::T2);
}

/// (i) IBD by the passive read: no onset, and no hook reaches the latching `is_ibd()` (the
/// probe counts one when a hook's scope does).
#[test]
fn i_ibd_by_the_passive_read_admits_nothing() {
    let t = T::with(
        RigConfig {
            after_genesis: Some(86_400),
            ..RigConfig::default()
        },
        true,
    );
    let net = stalled(&t, 1, 15_131);
    assert!(t.lock(0, &net[0]));
    t.ticks(0, 2 * HOUR, 300);
    assert_eq!(t.onset_since(), None);
    assert!(t.events().is_empty());
    assert_eq!(t.latching_reads(), 0);
    let latched = t.with_v(|v| !v.chain.is_ibd_private_unlatched(v.now(), true));
    assert!(!latched, "the IBD latch is unchanged");
    let probed = t.with_v(|v| {
        let _scope = HookScope::enter();
        assert!(v.is_ibd());
        latching_ibd_in_hook()
    });
    assert_eq!(probed, 1, "the probe sees a latching read inside a hook");
}

/// (j) Episode emission: repeated ticks on an unchanged failing floor, then a higher lock whose
/// path still holds the rejected ancestor, then more ticks: one first alert, a re-alert after
/// 3,600 s, at most 3 + 1 events (bundle requests), and the latch holds the first event.
#[test]
fn j_one_episode_alerts_once_then_hourly() {
    let t = T::new();
    let main = line(&t.rig, &t.rig.genesis, 5, 15_141);
    t.accept(0, &main[0]);
    t.accept(0, &main[1]);
    t.rig.headers(&main[2..]);
    t.fails.always(&main[2]);
    t.submit(0, &main[2]);
    assert!(t.lock(0, &main[2]));
    let mut events = t.events();
    let info = first(&events).clone();
    t.ticks(0, 3_000, 300);
    assert!(t.lock(3_000, &main[4]));
    assert_eq!(floor(&t.rig), Some(t.rig.id(&main[4])));
    t.ticks(3_000, 3 * HOUR - 1, 300);
    events.extend(t.events());
    assert_eq!(firsts(&events), 1);
    assert!(events.len() <= 4, "{events:?}");
    assert!(matches!(&events[1], DivergenceEvent::Realert(i) if i.episode == info.episode));
    assert_eq!(events.len(), 3, "first, then re-alerts at 1 h and 2 h");
    assert!(events.iter().all(|e| info_of(e).episode == info.episode));
    assert_eq!(t.latch(), Some(info));
    assert_eq!(t.tw(Tripwire::episode_count), 1);
}

/// (j) A T1 episode ends at resolution: its frozen floor X connects (the failure cleared, the
/// next reset retries it), and the next observation ends the episode. A later, unrelated T1 on Z
/// under a lock on Z is a second episode: a second `First`.
#[test]
fn j_a_resolved_t1_episode_ends_and_a_later_t1_is_a_new_episode() {
    let t = T::new();
    let main = connect_failure(&t, 15_145);
    let (x, z) = (&main[2], &main[3]);
    assert!(t.lock(10, x));
    let mut events = t.events();
    let info = first(&events).clone();
    assert_eq!(t.t1_episode(), Some(info.episode.clone()));
    t.fails.clear(x);
    t.tick(20);
    assert!(t.active(x), "the reset retried X and it connected");
    t.tick(30);
    assert_eq!(t.t1_episode(), None, "the frozen floor X is active: resolved");
    t.fails.always(z);
    t.submit(40, z);
    assert_eq!(t.rejection(z), Some(RejectionStage::Connect));
    assert!(t.lock(50, z));
    events.extend(t.events());
    assert_eq!(firsts(&events), 2, "{events:?}");
    assert_eq!(t.tw(Tripwire::episode_count), 2);
    let second = info_of(&events[1]);
    assert_eq!((second.kind, second.hash), (DivergenceKind::T1, z.hash()));
    assert_ne!(second.episode, info.episode);
    assert_eq!(t.t1_episode(), Some(second.episode.clone()));
}

/// (j) SPORK_19 off ends a live T1 episode at the next observation.
#[test]
fn j_spork_off_ends_the_t1_episode() {
    let t = T::new();
    let main = connect_failure(&t, 15_147);
    assert!(t.lock(10, &main[2]));
    let info = first(&t.events()).clone();
    assert_eq!(t.t1_episode(), Some(info.episode));
    let sporks = t.rig.sporks.as_ref().expect("a fresh rig");
    sporks.set(false);
    intake(&t.rig);
    t.tick(20);
    assert_eq!(t.t1_episode(), None);
}

/// (j2) A T2 episode ends when its onset lock's block connects. A later stall on a new lock that
/// shares no hash with it trips T2 again: a second `First`, a second episode.
#[test]
fn j2_a_resolved_t2_episode_ends_and_a_later_stall_is_a_new_episode() {
    let t = T::new();
    let net = stalled(&t, 2, 15_157);
    assert!(t.lock(0, &net[0]));
    t.tick(0);
    t.tick(HOUR);
    let mut events = t.events();
    let info = first(&events).clone();
    assert_eq!((info.kind, info.onset_cl), (DivergenceKind::T2, Some(net[0].hash())));
    t.accept(HOUR + 100, &net[0]);
    t.tick(HOUR + 100);
    assert_eq!(t.onset_since(), None, "the onset lock's block connected");
    t.rig.headers(&net[1..]);
    assert!(t.lock(HOUR + 200, &net[1]));
    assert_eq!(t.onset_since(), Some(HOUR + 200));
    t.ticks(HOUR + 200, 2 * HOUR + 199, 600);
    assert!(t.events().is_empty());
    t.tick(2 * HOUR + 200);
    events.extend(t.events());
    assert_eq!(firsts(&events), 2, "{events:?}");
    assert_eq!(t.tw(Tripwire::episode_count), 2);
    let second = info_of(&events[1]);
    assert_eq!((second.cl_hash, second.onset_cl), (net[1].hash(), Some(net[1].hash())));
    assert_ne!(second.episode, info.episode);
}

/// (j2) T2's frozen episode: a known lock block whose data never arrives while its ancestors
/// arrive every 10 min, plus a Pending → known transition, all after the deadline: one key
/// throughout, alerts at onset + 3,600 s and then hourly, at most 3 + 1 events.
#[test]
fn j2_a_t2_episode_key_is_frozen() {
    let t = T::new();
    let a1 = kid(&t.rig, &t.rig.genesis, 15_151);
    t.accept(0, &a1);
    let net = line(&t.rig, &a1, 12, 15_152);
    t.rig.headers(&net[..10]);
    assert!(t.lock(0, &net[9]));
    let key = t.tw(|w| w.onset()).expect("an onset").key;
    assert_eq!(key, (1, a1.hash()));
    let mut seen = Vec::new();
    let mut now = 0;
    while now < 4 * HOUR {
        now += 600;
        if (HOUR + 600..=HOUR + 9 * 600).contains(&now) {
            t.accept(now, &net[((now - HOUR) / 600 - 1) as usize]);
        }
        if now == 9_600 {
            assert!(t.lock(now, &net[11]));
        }
        if now == 10_200 {
            t.at(now);
            t.rig.headers(&net[10..]);
        }
        t.tick(now);
        assert_eq!(t.tw(|w| w.onset().map(|o| o.key)), Some(key), "{now}");
        seen.extend(t.events().into_iter().map(|e| (now, e)));
    }
    assert!(t.active(&net[8]) && !t.active(&net[9]));
    let times: Vec<u64> = seen.iter().map(|(at, _)| *at).collect();
    assert_eq!(times, [HOUR, 2 * HOUR, 3 * HOUR, 4 * HOUR]);
    let episode = info_of(&seen[0].1).episode.clone();
    assert_eq!(episode, format!("T2-1-{}", &a1.hash().to_string()[..12]));
    assert!(seen.iter().all(|(_, e)| info_of(e).episode == episode));
    assert_eq!(firsts(&seen.into_iter().map(|(_, e)| e).collect::<Vec<_>>()), 1);
}

/// (j3) A T1 episode on floor F, with F above the rejected block X, then 1,024 unrelated
/// rejections that evict X's entry while F stays off our chain, then more ticks: the key stays
/// the first event's; one first alert, hourly re-alerts. Then the fresh Connect rejection is on W,
/// X's descendant below F, so W is on the unresolved floor's path (the spec's "descendant of F"
/// cannot happen while F is itself off-chain above X). Under a higher lock G it must produce a T1
/// event: it carries the frozen key and is no second first alert, although none of its hashes is
/// the first event's (a key recomputed from the current map or event makes a new episode).
#[test]
fn j3_the_t1_key_survives_eviction_and_a_new_rejection() {
    let t = T::new();
    let main = line(&t.rig, &t.rig.genesis, 6, 15_161);
    t.accept(0, &main[0]);
    let (x, w, f, g) = (&main[1], &main[2], &main[3], &main[4]);
    t.rig.headers(&main[1..5]);
    t.submit(0, w);
    t.fails.always(x);
    t.submit(0, x);
    assert!(t.lock(0, f));
    let mut events = t.events();
    let info = first(&events).clone();
    assert_eq!((info.cl_hash, info.floor_hash, info.hash), (f.hash(), Some(f.hash()), x.hash()));

    t.at(600);
    t.rig.on_v(|v| {
        let err = BlockValidationError::new(BlockValidationResult::Consensus, "unrelated");
        for n in 0..REJECTED_CAPACITY as u32 {
            let id = BlockId(u32::MAX - n);
            v.tripwire.record(id, BlockHash::from([7; 32]), n, RejectionStage::Connect, &err, 600);
        }
        Ok(())
    });
    assert_eq!(t.rejection(x), None, "X's entry is evicted");
    assert_eq!(t.t1_episode(), Some(info.episode.clone()));
    t.ticks(600, HOUR, 600);
    events.extend(t.events());
    assert_eq!(t.t1_episode(), Some(info.episode.clone()));

    let before = t.tw(Tripwire::episode_events);
    t.fails.clear(x);
    t.fails.always(w);
    assert!(t.lock(4_000, g));
    assert!(t.active(x) && !t.active(f), "W fails, so F stays off our chain");
    assert_eq!(t.rejection(w), Some(RejectionStage::Connect));
    let after = t.tw(Tripwire::episode_events);
    assert_eq!(after.len(), 1, "no new episode");
    assert_eq!(after[0].0, info.episode, "the frozen key");
    assert!(after[0].2 > before[0].2, "the forced T1 event occurred");
    t.ticks(4_000, 3 * HOUR - 1, 600);
    events.extend(t.events());
    assert_eq!(firsts(&events), 1, "{events:?}");
    assert!(events.len() <= 4, "{events:?}");
    assert!(
        events[1..]
            .iter()
            .all(|e| matches!(e, DivergenceEvent::Realert(i) if i.episode == info.episode))
    );
    assert_eq!(events.len(), 3, "first, then re-alerts at 1 h and 2 h");
    assert_eq!(t.latch(), Some(info));
}

/// (j4) T1 then T2 on one divergence: a Connect rejection of X and a lock on X trip T1; C′ then
/// holds for 60 min with a new lock every 150 s, and T2's trip (its `onset_cl` X, its floor still
/// X) joins the T1 episode. One episode, one first alert, the T1 episode's cadence. Joins through
/// `onset_cl` alone are (q2) (ii)'s and `j4_t2_then_t1_joins_through_the_rejected_hash`'s.
#[test]
fn j4_t1_then_t2_is_one_episode() {
    let t = T::new();
    let main = connect_failure(&t, 15_171);
    let x = &main[2];
    let past = line(&t.rig, x, 30, 15_172);
    assert!(t.lock(0, x));
    let mut seen: Vec<(u64, DivergenceEvent)> = t.events().into_iter().map(|e| (0, e)).collect();
    let info = first(&seen.iter().map(|(_, e)| e.clone()).collect::<Vec<_>>()).clone();
    assert_eq!(info.kind, DivergenceKind::T1);
    for (i, m) in past.iter().enumerate().take(28) {
        let now = 150 * (i as u64 + 1);
        assert!(t.lock(now, m));
        t.tick(now);
        seen.extend(t.events().into_iter().map(|e| (now, e)));
    }
    assert!(seen.iter().any(|(_, e)| matches!(e, DivergenceEvent::Realert(i)
        if i.kind == DivergenceKind::T2 && i.onset_cl == Some(x.hash()))));
    let events: Vec<_> = seen.iter().map(|(_, e)| e.clone()).collect();
    assert_eq!(firsts(&events), 1);
    assert!(events.iter().all(|e| info_of(e).episode == info.episode));
    let times: Vec<u64> = seen.iter().map(|(at, _)| *at).collect();
    assert_eq!(times, [0, HOUR]);
    assert_eq!(t.tw(Tripwire::episode_count), 1);
}

/// (j4) T2 then T1: a lock on X with X's block withheld trips T2 with `onset_cl` X; a newer lock
/// arrives (Pending, so the floor stays X); X's block then fails ConnectBlock and T1, with the
/// newer lock, joins the T2 episode through X (its `onset_cl` and floor). One episode, one first
/// alert.
#[test]
fn j4_t2_then_t1_is_one_episode() {
    let t = T::new();
    let main = line(&t.rig, &t.rig.genesis, 3, 15_181);
    t.accept(0, &main[0]);
    let (x, y) = (&main[1], &main[2]);
    t.rig.headers(std::slice::from_ref(x));
    assert!(t.lock(0, x));
    t.tick(HOUR);
    let events = t.events();
    let info = first(&events).clone();
    assert_eq!((info.kind, info.onset_cl), (DivergenceKind::T2, Some(x.hash())));
    assert!(t.lock(HOUR + 100, y));
    t.fails.always(x);
    t.submit(HOUR + 200, x);
    t.tick(HOUR + 200);
    assert_eq!(t.t1_episode(), Some(info.episode.clone()));
    t.ticks(HOUR + 200, 2 * HOUR, 600);
    let later = t.events();
    assert_eq!(firsts(&later), 0, "{later:?}");
    assert!(later.iter().all(|e| info_of(e).episode == info.episode));
    assert_eq!(t.tw(Tripwire::episode_count), 1);
}

/// (j4) T2 on X with X's header unknown (no floor), then a newer lock Y whose header is known
/// (the floor moves to Y), then X fails ConnectBlock: the T1 event's hashes are {Y, Y, X} and it
/// joins the T2 episode only through its rejected block X = the episode's `onset_cl`. One
/// episode, one marker request (one `First`), one first alert.
#[test]
fn j4_t2_then_t1_joins_through_the_rejected_hash() {
    let t = T::new();
    let main = line(&t.rig, &t.rig.genesis, 3, 15_185);
    t.accept(0, &main[0]);
    let (x, y) = (&main[1], &main[2]);
    assert!(t.lock(0, x));
    t.tick(0);
    t.tick(HOUR);
    let mut events = t.events();
    let info = first(&events).clone();
    assert_eq!(info.kind, DivergenceKind::T2);
    assert_eq!((info.cl_hash, info.floor_hash, info.onset_cl), (x.hash(), None, Some(x.hash())));
    t.rig.headers(&main[1..3]);
    assert!(t.lock(HOUR + 100, y));
    assert_eq!(floor(&t.rig), Some(t.rig.id(y)), "the floor moved to Y");
    t.fails.always(x);
    t.submit(HOUR + 200, x);
    t.tick(HOUR + 200);
    assert_eq!(t.rejection(x), Some(RejectionStage::Connect));
    assert_eq!(t.t1_episode(), Some(info.episode.clone()), "T1 joined the T2 episode");
    let joined = t.tw(Tripwire::episode_events);
    assert_eq!(joined.len(), 1, "no new episode");
    assert!(joined[0].2 > 0, "the T1 event occurred");
    t.ticks(HOUR + 200, 3 * HOUR, 600);
    events.extend(t.events());
    assert_eq!(firsts(&events), 1, "{events:?}");
    assert!(events.iter().all(|e| info_of(e).episode == info.episode));
    assert_eq!(t.tw(Tripwire::episode_count), 1);
    assert_eq!(t.latch(), Some(info));
}

/// (j4) The same join against a restored T2 episode whose only hash shared with the T1 event is
/// its `onset_cl` X (its `cl_hash` is unrelated, its floor absent): no `First`, one episode.
#[test]
fn j4_restored_t2_then_t1_joins_through_the_rejected_hash() {
    let t = T::new();
    let main = line(&t.rig, &t.rig.genesis, 4, 15_186);
    t.accept(0, &main[0]);
    let (x, y) = (&main[1], &main[2]);
    let mut restored = live_info(DivergenceKind::T2, BlockHash::from([9; 32]));
    restored.onset_cl = Some(x.hash());
    t.restore(restored.clone());
    t.rig.headers(&main[1..3]);
    assert!(t.lock(10, y));
    assert!(t.events().is_empty(), "Y is none of the restored episode's hashes");
    t.fails.always(x);
    t.submit(20, x);
    t.tick(20);
    assert_eq!(t.rejection(x), Some(RejectionStage::Connect));
    assert_eq!(t.t1_episode(), Some(restored.episode.clone()));
    // Up to an hour: Y's own T2 onset (since 10 s) would trip a separate episode after that.
    t.ticks(20, HOUR, 600);
    let events = t.events();
    assert_eq!(firsts(&events), 0, "{events:?}");
    assert!(!events.is_empty());
    assert!(events.iter().all(|e| info_of(e).episode == restored.episode));
    assert_eq!(t.tw(Tripwire::episode_count), 1);
}

/// (j4) Two unrelated T1 events, with different rejected blocks and different locks: a restored
/// T1 episode (live for the whole process) and a live T1 on X are two episodes. A negative
/// control: it passes with or without the rejected-hash join.
#[test]
fn j4_unrelated_t1_events_are_two_episodes() {
    let t = T::new();
    let main = connect_failure(&t, 15_187);
    let x = &main[2];
    let mut restored = live_info(DivergenceKind::T1, BlockHash::from([8; 32]));
    restored.floor_hash = Some(restored.cl_hash);
    restored.hash = BlockHash::from([7; 32]);
    restored.stage = Some(RejectionStage::Connect);
    t.restore(restored.clone());
    assert!(t.lock(10, x));
    let events = t.events();
    let info = first(&events);
    assert_eq!((info.kind, info.hash, info.cl_hash), (DivergenceKind::T1, x.hash(), x.hash()));
    assert_ne!(info.episode, restored.episode);
    assert_eq!(t.tw(Tripwire::episode_count), 2);
}

/// X's header over a body with a second coinbase, its merkle pass skipped: a Check-stage entry
/// for X, no status change.
fn check_reject(t: &T, x: &Mined, salt: u32) {
    let mut txs = x.block.txs().to_vec();
    txs.push(coinbase(&cb_script(x.height, salt)));
    let forged = Arc::new(Block::new(*x.block.header(), txs));
    let r = t.rig.v.submit_block(forged, true, None, true, true).recv();
    assert_eq!(reject_reason(&r.expect("V replies")), "bad-cb-multiple");
    assert_eq!(t.rejection(x), Some(RejectionStage::Check));
}

/// A restored T1 episode whose only hash shared with this process's events is its rejected
/// block `x`.
fn restored_t1_on(x: &Mined) -> DivergenceInfo {
    let mut restored = live_info(DivergenceKind::T1, BlockHash::from([8; 32]));
    restored.floor_hash = Some(restored.cl_hash);
    restored.hash = x.hash();
    restored.height = x.height;
    restored.stage = Some(RejectionStage::Connect);
    restored
}

/// (j4, OR3-1) An onset on W = h4, X's descendant (data missing): the first off-chain lock names
/// the network's tip, not the first block above ours. With `moved`, a newer lock lands before
/// X's body fails ConnectBlock, so the T1 event's hashes are {h5, h5, X}. Locks then come every
/// 150 s, and T2 trips at 60 min on a lock past X with `onset_cl` W, its reason naming X's entry.
/// With `restored`, a restored T1 episode on X is seeded first. Returns the events.
fn onset_then_t1_then_t2(moved: bool, restored: bool, salt: u32) -> (T, Vec<DivergenceEvent>) {
    let t = T::new();
    let main = line(&t.rig, &t.rig.genesis, 40, salt);
    t.accept(0, &main[0]);
    let (x, w) = (&main[1], &main[3]);
    if restored {
        t.restore(restored_t1_on(x));
    }
    t.rig.headers(&main[1..40]);
    assert!(t.lock(0, w));
    assert_eq!(t.tw(|tw| tw.onset().map(|o| o.cl)), Some(w.hash()));
    assert!(t.events().is_empty());
    if moved {
        assert!(t.lock(150, &main[4]));
    }
    t.fails.always(x);
    t.submit(200, x);
    t.tick(200);
    let mut seen = t.events();
    assert!(seen.first().is_some_and(|e| info_of(e).kind == DivergenceKind::T1), "{seen:?}");
    for k in 0..22u64 {
        let now = 300 + 150 * k;
        assert!(t.lock(now, &main[5 + k as usize]));
        t.tick(now);
        seen.extend(t.events());
    }
    t.tick(HOUR);
    seen.extend(t.events());
    let t2_events: u64 = t.tw(Tripwire::episode_events).iter().map(|(_, all, t1)| all - t1).sum();
    assert!(t2_events > 0, "T2 tripped");
    (t, seen)
}

/// (j4, OR3-1) T1 on X under one lock, the best lock moving, then T2 with onset W above X whose
/// reason names X's entry: T2 joins the T1 episode through X. One episode, one `First`. The
/// control, with no lock in between, joins through W.
#[test]
fn j4_t1_then_t2_with_a_moved_lock_joins_through_the_reason() {
    for moved in [false, true] {
        let (t, seen) = onset_then_t1_then_t2(moved, false, 99_001);
        assert_eq!((firsts(&seen), t.tw(Tripwire::episode_count)), (1, 1), "{moved}: {seen:#?}");
        let first = info_of(&seen[0]).episode.clone();
        assert!(seen.iter().all(|e| info_of(e).episode == first), "{moved}: {seen:#?}");
    }
}

/// (j4, OR3-1) The same against a restored T1 episode on X: T1 and then T2 join it, no `First`.
#[test]
fn j4_restored_t1_then_t2_with_a_moved_lock_joins_through_the_reason() {
    let (t, seen) = onset_then_t1_then_t2(true, true, 99_003);
    assert_eq!((firsts(&seen), t.tw(Tripwire::episode_count)), (0, 1), "{seen:#?}");
    assert!(seen.iter().all(|e| info_of(e).restored || !matches!(e, DivergenceEvent::First(_))));
}

/// (j4, OR3-1) The reverse order: X is a Check-stage entry (no T1); T2 trips with onset W and its
/// reason names X, so X is one of its join hashes; a newer lock arrives, then X's body fails
/// ConnectBlock, and T1 {newer, newer, X} joins the T2 episode through X. One episode.
#[test]
fn j4_t2_naming_x_then_t1_under_a_newer_lock_is_one_episode() {
    let t = T::new();
    let main = line(&t.rig, &t.rig.genesis, 40, 99_005);
    t.accept(0, &main[0]);
    let (x, w) = (&main[1], &main[3]);
    t.rig.headers(&main[1..40]);
    check_reject(&t, x, 99_006);
    assert!(t.lock(0, w));
    for k in 0..24u64 {
        let now = 150 * (k + 1);
        assert!(t.lock(now, &main[4 + k as usize]));
        t.tick(now);
    }
    let mut events = t.events();
    let info = first(&events).clone();
    assert_eq!((info.kind, info.stage), (DivergenceKind::T2, Some(RejectionStage::Check)));
    assert_eq!((info.hash, info.height, info.onset_cl), (x.hash(), x.height, Some(w.hash())));
    assert_ne!(info.cl_hash, w.hash());
    let newer = &main[29];
    assert!(t.lock(HOUR + 100, newer));
    t.fails.always(x);
    t.submit(HOUR + 200, x);
    t.tick(HOUR + 200);
    assert_eq!(t.rejection(x), Some(RejectionStage::Connect));
    assert_eq!(t.t1_episode(), Some(info.episode.clone()), "T1 joined the T2 episode");
    let joined = t.tw(Tripwire::episode_events);
    assert_eq!(joined.len(), 1, "no new episode");
    assert!(joined[0].2 > 0, "the T1 event occurred");
    t.ticks(HOUR + 200, 3 * HOUR, 600);
    events.extend(t.events());
    assert_eq!(firsts(&events), 1, "{events:?}");
    assert!(events.iter().all(|e| info_of(e).episode == info.episode));
    assert_eq!(t.latch(), Some(info));
}

/// (j4, OR3-1) The reverse order against a restored T2 episode whose reason named X (`stage`
/// Check, `hash` X) and whose `cl_hash` and `onset_cl` are unrelated: X fails ConnectBlock under
/// a lock, and T1 joins the restored episode through X. No `First`.
#[test]
fn j4_restored_t2_naming_x_then_t1_joins_through_x() {
    let t = T::new();
    let main = line(&t.rig, &t.rig.genesis, 6, 99_007);
    t.accept(0, &main[0]);
    let (x, newer) = (&main[1], &main[5]);
    let mut restored = live_info(DivergenceKind::T2, BlockHash::from([9; 32]));
    restored.onset_cl = Some(BlockHash::from([6; 32]));
    restored.stage = Some(RejectionStage::Check);
    restored.hash = x.hash();
    restored.height = x.height;
    t.restore(restored.clone());
    t.rig.headers(&main[1..6]);
    t.fails.always(x);
    t.submit(0, x);
    assert_eq!(t.rejection(x), Some(RejectionStage::Connect));
    assert!(t.lock(10, newer));
    assert_eq!(t.t1_episode(), Some(restored.episode.clone()));
    t.ticks(10, HOUR - 1, 600);
    let events = t.events();
    assert_eq!(firsts(&events), 0, "{events:?}");
    assert!(!events.is_empty());
    assert!(events.iter().all(|e| info_of(e).episode == restored.episode));
    assert_eq!(t.tw(Tripwire::episode_count), 1);
}

/// (j4, RC1-1) X's body withheld past W's hour, with a lock every 150 s: T2 trips with onset W
/// above X and no entry on its path (`stage` none), so none of its hashes is X. With
/// `newer_lock`, the best lock moves before X's body fails ConnectBlock, so the T1 event shares
/// no hash with the T2 episode. Returns the events and X.
fn t2_without_stage_then_t1(newer_lock: bool, salt: u32) -> (T, Vec<DivergenceEvent>) {
    let t = T::new();
    let main = line(&t.rig, &t.rig.genesis, 40, salt);
    t.accept(0, &main[0]);
    let (x, w) = (&main[1], &main[3]);
    t.rig.headers(&main[1..40]);
    assert!(t.lock(0, w));
    for k in 0..24u64 {
        let now = 150 * (k + 1);
        assert!(t.lock(now, &main[4 + k as usize]));
        t.tick(now);
    }
    let mut events = t.events();
    let info = first(&events).clone();
    assert_eq!((info.kind, info.stage, info.onset_cl), (DivergenceKind::T2, None, Some(w.hash())));
    assert_ne!(info.hash, x.hash());
    if newer_lock {
        assert!(t.lock(HOUR + 100, &main[29]));
    }
    t.fails.always(x);
    t.submit(HOUR + 200, x);
    t.tick(HOUR + 200);
    assert_eq!(t.rejection(x), Some(RejectionStage::Connect));
    assert_eq!(t.t1_episode(), Some(info.episode.clone()), "T1 joined the T2 episode");
    t.ticks(HOUR + 200, 3 * HOUR, 600);
    events.extend(t.events());
    assert!(events.iter().all(|e| info_of(e).episode == info.episode), "{events:#?}");
    (t, events)
}

/// (j4, RC1-1) T2 with no stage, then X on its onset lock's path fails under a newer lock: T1
/// joins the T2 episode through the onset path. One episode, one `First`.
#[test]
fn j4_t2_without_stage_then_t1_under_a_newer_lock_joins_on_the_onset_path() {
    let (t, events) = t2_without_stage_then_t1(true, 99_105);
    assert_eq!((firsts(&events), t.tw(Tripwire::episode_count)), (1, 1), "{events:#?}");
}

/// (j4, RC1-1) The control with no newer lock: T1 joins through the shared lock, as before.
#[test]
fn j4_t2_without_stage_then_t1_under_the_same_lock_joins() {
    let (t, events) = t2_without_stage_then_t1(false, 99_101);
    assert_eq!((firsts(&events), t.tw(Tripwire::episode_count)), (1, 1), "{events:#?}");
}

/// (j4, RC1-1) A restored T2 marker with no stage and onset W (its `cl_hash` unrelated, no
/// floor), then X below W fails ConnectBlock under a newer lock Y: T1 {Y, Y, X} shares no hash
/// with it, and joins it through W's path. No `First`, one episode.
#[test]
fn j4_restored_t2_without_stage_then_t1_joins_on_the_onset_path() {
    let t = T::new();
    let main = line(&t.rig, &t.rig.genesis, 6, 99_107);
    t.accept(0, &main[0]);
    let (x, w, newer) = (&main[1], &main[3], &main[5]);
    let mut restored = live_info(DivergenceKind::T2, BlockHash::from([9; 32]));
    restored.onset_cl = Some(w.hash());
    t.restore(restored.clone());
    t.rig.headers(&main[1..6]);
    assert!(t.lock(10, newer));
    assert!(t.events().is_empty(), "Y is none of the restored episode's hashes");
    t.fails.always(x);
    t.submit(20, x);
    t.tick(20);
    assert_eq!(t.rejection(x), Some(RejectionStage::Connect));
    assert_eq!(t.t1_episode(), Some(restored.episode.clone()));
    // Up to an hour: Y's own T2 onset (since 10 s) would trip a separate episode after that.
    t.ticks(20, HOUR - 1, 600);
    let events = t.events();
    assert_eq!(firsts(&events), 0, "{events:?}");
    assert!(!events.is_empty());
    assert!(events.iter().all(|e| info_of(e).episode == restored.episode));
    assert_eq!(t.tw(Tripwire::episode_count), 1);
}

/// (j4, RC1-1) The negative: a restored T2 marker with no stage and onset W on a side branch
/// (W's header known, its block at X's height another block), then X on our chain fails
/// ConnectBlock under a lock: X is off W's path, so T1 is a second episode.
#[test]
fn j4_t1_off_the_onset_path_is_a_second_episode() {
    let t = T::new();
    let main = line(&t.rig, &t.rig.genesis, 6, 99_109);
    t.accept(0, &main[0]);
    let side = line(&t.rig, &main[0], 3, 99_110);
    let (x, w, newer) = (&main[1], &side[2], &main[5]);
    assert_eq!(side[0].height, x.height);
    let mut restored = live_info(DivergenceKind::T2, BlockHash::from([9; 32]));
    restored.onset_cl = Some(w.hash());
    t.restore(restored.clone());
    t.rig.headers(&side);
    t.rig.headers(&main[1..6]);
    assert!(t.lock(10, newer));
    t.fails.always(x);
    t.submit(20, x);
    t.tick(20);
    assert_eq!(t.rejection(x), Some(RejectionStage::Connect));
    let events = t.events();
    let info = first(&events);
    assert_eq!((info.kind, info.hash), (DivergenceKind::T1, x.hash()));
    assert_ne!(info.episode, restored.episode);
    assert_eq!((firsts(&events), t.tw(Tripwire::episode_count)), (1, 2), "{events:#?}");
}

/// (j4) Absent hashes: two unrelated T2 events, both with no floor, on different locks, are two
/// episodes (the first a restored one, live for the whole process).
#[test]
fn j4_absent_floors_never_join() {
    let t = T::new();
    let net = stalled(&t, 1, 15_191);
    let mut restored = live_info(DivergenceKind::T2, BlockHash::from([9; 32]));
    restored.onset_cl = Some(restored.cl_hash);
    t.restore(restored);
    assert!(t.lock(0, &net[0]));
    t.tick(0);
    t.tick(HOUR);
    let events = t.events();
    let info = first(&events);
    assert_eq!(info.floor_hash, None);
    assert_eq!(t.tw(Tripwire::episode_count), 2);
}

fn live_info(kind: DivergenceKind, cl_hash: BlockHash) -> DivergenceInfo {
    DivergenceInfo {
        episode: format!("{kind:?}-9-{}", &cl_hash.to_string()[..12]),
        kind,
        stage: None,
        height: 9,
        hash: cl_hash,
        cl_hash,
        floor_hash: None,
        onset_cl: None,
        clsig: [9; 96],
        reason: NO_CONNECT.to_owned(),
        h_obs: 1,
        restored: false,
        first_event_unix: 1,
        seq: 0,
    }
}

/// (k) Enforcement with the floor 10,000 blocks above the tip and a full map: T1's evaluation
/// takes at most 2 × 1,024 skip-list lookups.
#[test]
fn k_t1_lookups_are_bounded_by_the_map() {
    let t = T::with(
        RigConfig {
            no_check_block_index: true,
            ..RigConfig::default()
        },
        true,
    );
    let a1 = kid(&t.rig, &t.rig.genesis, 15_201);
    t.accept(0, &a1);
    let far = line(&t.rig, &a1, 10_000, 15_202);
    for chunk in far.chunks(2_000) {
        t.rig.headers(chunk);
    }
    let (g, a) = (t.rig.id(&t.rig.genesis), t.rig.id(&a1));
    t.rig.on_v(move |v| {
        let err = BlockValidationError::new(BlockValidationResult::Consensus, "unrelated");
        for n in 0..REJECTED_CAPACITY as u32 - 2 {
            let id = BlockId(u32::MAX - n);
            v.tripwire.record(id, BlockHash::from([7; 32]), n, RejectionStage::Connect, &err, 0);
        }
        // On the floor's path but on our chain: two lookups each.
        v.tripwire.record(g, BlockHash::from([7; 32]), 0, RejectionStage::Connect, &err, 0);
        v.tripwire.record(a, BlockHash::from([7; 32]), 1, RejectionStage::Accept, &err, 0);
        Ok(())
    });
    assert!(t.lock(0, &far[9_999]));
    assert!(t.events().is_empty());
    let lookups = t.tw(Tripwire::lookups);
    assert!(lookups <= 2 * REJECTED_CAPACITY as u64, "{lookups}");
    assert_eq!(lookups, REJECTED_CAPACITY as u64 + 2, "every entry was tested");
}

/// (l) A Mutated failure inserts nothing at any of the three sites. The P2P CheckBlock site is
/// driven end to end; for AcceptBlock and ConnectBlock this asserts the shared filter in
/// `record`, which every site goes through, rather than each site's own Mutated guard.
#[test]
fn l_mutated_failures_insert_nothing() {
    let t = T::new();
    let a1 = kid(&t.rig, &t.rig.genesis, 15_211);
    t.accept(0, &a1);
    // P2P CheckBlock: a duplicated transaction (`bad-txns-duplicate`).
    let cb = coinbase(&cb_script(2, 15_212));
    let one = coinbase(&cb_script(2, 15_213));
    let twin = coinbase(&cb_script(2, 15_214));
    let dup = Mined {
        block: mine(&t.rig.p, a1.hash(), a1.time() + 1, vec![cb, one, twin.clone(), twin]),
        height: 2,
    };
    t.rig.headers(std::slice::from_ref(&dup));
    let r = t.submit(0, &dup);
    assert!(
        matches!(&r.state, BlockState::Invalid(e) if e.result() == BlockValidationResult::Mutated),
        "{r:?}"
    );
    // AcceptBlock's and ConnectBlock's sites skip Mutated before the shared insert, whose own
    // filter is checked on V directly: a connector's Mutated result is no Core outcome, and
    // without a FAILED mark ABC would retry the block forever.
    let id = t.rig.id(&dup);
    t.rig.on_v(move |v| {
        let err = BlockValidationError::new(BlockValidationResult::Mutated, "mutated");
        for stage in [RejectionStage::Accept, RejectionStage::Connect] {
            v.tripwire_reject(id, stage, &err);
        }
        Ok(())
    });
    assert!(t.tw(|w| w.rejected().is_empty()));
}

/// (m1) A lock whose height mismatches its block, known at intake within 48 blocks of our tip:
/// no latch, no alert past 3,600 s; Core's best, known best and floor as with the tripwire off;
/// the error line once.
#[test]
fn m1_a_height_mismatch_at_intake_never_trips() {
    install_accept_log();
    let run = |armed: bool, salt: u32| {
        let t = T::with(RigConfig::default(), armed);
        let net = stalled(&t, 1, salt);
        t.rig.headers(&net);
        let wrong = lock_at(&net[0], 7);
        assert!(!t.lock_sig(0, &wrong), "a mismatch is not relayed");
        t.ticks(0, HOUR + 600, 300);
        let state = (cl(&t.rig).best.clone(), cl(&t.rig).best_known.clone(), floor(&t.rig));
        (t, net, wrong, state)
    };
    let (_, _, _, disarmed) = run(false, 15_221);
    let (t, net, wrong, armed) = run(true, 15_221);
    assert_eq!(armed, disarmed);
    assert!(t.events().is_empty());
    assert_eq!(t.latch(), None);
    let line = format!(
        "ChainLock height mismatch: CLSIG ({wrong}) names {}, indexed at height {}",
        net[0].hash(),
        net[0].height
    );
    assert_eq!(lines_with(&line).len(), 1, "{line}");
}

/// (m2) A Pending lock whose late header reveals the mismatch: the onset clears there, and
/// nothing trips; Core's best, known best and floor as with the tripwire off; the error line
/// once.
#[test]
fn m2_a_late_header_mismatch_clears_the_onset() {
    install_accept_log();
    let run = |armed: bool| {
        let t = T::with(RigConfig::default(), armed);
        let net = stalled(&t, 1, 15_231);
        let wrong = lock_at(&net[0], 5);
        assert!(t.lock_sig(0, &wrong));
        t.tick(0);
        if armed {
            assert_eq!(t.onset_since(), Some(0));
        }
        t.at(600);
        t.rig.headers(&net);
        t.tick(600);
        assert_eq!(t.onset_since(), None);
        t.ticks(600, 2 * HOUR, 300);
        let state = (cl(&t.rig).best.clone(), cl(&t.rig).best_known.clone(), floor(&t.rig));
        (t, net, wrong, state)
    };
    let (_, _, _, disarmed) = run(false);
    let (t, net, wrong, armed) = run(true);
    assert_eq!(armed, disarmed);
    assert!(t.events().is_empty());
    assert_eq!(t.latch(), None);
    let line = format!("ChainLock height mismatch: CLSIG ({wrong}) names {}", net[0].hash());
    assert_eq!(lines_with(&line).len(), 1);
}

/// What (n) compares between an armed and a disarmed V.
#[derive(Debug, PartialEq)]
struct Snapshot {
    statuses: Vec<(BlockHash, u32)>,
    failed: Vec<BlockHash>,
    tip: Option<BlockHash>,
    relays: Vec<bool>,
    last_notify: Option<BlockHash>,
    queued: Vec<(BlockId, i32)>,
    signals: String,
    finished_ibd: bool,
    ibd: bool,
    warned: bool,
    misc: Vec<String>,
    ui: (Vec<(BlockHash, i32)>, Vec<BlockHash>),
}

fn snapshot(t: &T, relays: Vec<bool>) -> Snapshot {
    t.rig.sync();
    let (statuses, failed, tip, last_notify, finished_ibd, ibd, warned) = t.with_v(|v| {
        let hash = |id: BlockId| v.reader.entry(id).expect("indexed").hash;
        let statuses = v
            .chain
            .ids()
            .map(|id| (hash(id), v.reader.entry(id).expect("indexed").status.bits()))
            .collect();
        let failed: Vec<BlockId> =
            v.chain.node_index().expect("the index").failed_blocks().iter().collect();
        (
            statuses,
            failed.into_iter().map(hash).collect(),
            v.tip().map(hash),
            v.cl.last_notify.map(hash),
            !v.chain.is_ibd_private_unlatched(v.now(), true),
            v.chain.is_ibd_private_unlatched(v.now(), v.loading.any()),
            v.warned,
        )
    });
    Snapshot {
        statuses,
        failed,
        tip,
        relays,
        last_notify,
        queued: t.log.take().iter().map(|(id, s)| (*id, s.height)).collect(),
        signals: format!("{:?}", t.rig.rec.take()),
        finished_ibd,
        ibd,
        warned,
        misc: std::mem::take(&mut *t.log.misc.lock().expect("misc")),
        ui: (
            std::mem::take(&mut *t.log.ui.lock().expect("ui")),
            std::mem::take(&mut *t.log.tips.lock().expect("tips")),
        ),
    }
}

/// Runs `scenario` on an armed and a disarmed V and compares what Core's state and outputs are.
fn differential(cfg: impl Fn() -> RigConfig, scenario: impl Fn(&T) -> Vec<bool>) {
    let run = |armed: bool| {
        let t = T::with(cfg(), armed);
        let relays = scenario(&t);
        let events = t.events();
        (snapshot(&t, relays), events)
    };
    let (disarmed, none) = run(false);
    let (armed, events) = run(true);
    assert!(none.is_empty());
    assert_eq!(armed, disarmed, "the tripwire changed Core's state; it emitted {events:?}");
}

/// (n) The differential, on the added scenarios and the existing ones below, and
/// (`n_existing_scenarios_run_armed_and_disarmed`) on a set of the existing locks and
/// `evo_chain_cl` tests. The whole suites can also run disarmed by hand with
/// `DASHD_TEST_TRIPWIRE_DISABLED=1`.
#[test]
fn n_a_connect_stage_failure_under_a_lock_elsewhere() {
    differential(RigConfig::default, |t| {
        let main = line(&t.rig, &t.rig.genesis, 4, 15_241);
        let side = line(&t.rig, &t.rig.genesis, 2, 15_242);
        t.accept(0, &main[0]);
        t.accept(0, &main[1]);
        t.fails.always(&main[2]);
        t.submit(0, &main[2]);
        let mut relays = vec![t.lock(10, &main[2])];
        t.ticks(10, 2 * HOUR, 600);
        for m in &side {
            t.submit(2 * HOUR, m);
        }
        relays.push(t.lock(2 * HOUR, &side[1]));
        t.ticks(2 * HOUR, 4 * HOUR, 600);
        relays
    });
}

#[cfg(feature = "planted-bugs")]
#[test]
fn n_an_accept_stage_failure_under_a_lock_on_its_descendant() {
    differential(RigConfig::default, |t| {
        let main = line(&t.rig, &t.rig.genesis, 4, 15_251);
        t.accept(0, &main[0]);
        t.rig.headers(&main[1..]);
        t.rig.v.plant(Some(crate::Planted::RejectAcceptAtHeight(main[1].height)));
        t.submit(0, &main[1]);
        let relays = vec![t.lock(10, &main[3])];
        t.ticks(10, 2 * HOUR, 600);
        relays
    });
}

/// (n) S2-4's IBD boundary: an unfinished IBD latch, a backward node-clock correction that makes
/// the tip young enough to leave IBD, then enforcement onto a missing-data rival whose fork is
/// still too old.
#[test]
fn n_the_ibd_boundary() {
    let cfg = || RigConfig {
        after_genesis: Some(86_400),
        ..RigConfig::default()
    };
    differential(cfg, |t| {
        let main = super::branch(&t.rig.p, &t.rig.genesis, 2, 15_261);
        let rival = line(&t.rig, &t.rig.genesis, 3, 15_262);
        for m in &main {
            t.accept(0, m);
        }
        t.rig.headers(&rival);
        let latched = t.with_v(|v| !v.chain.is_ibd_private_unlatched(v.now(), true));
        assert!(!latched, "IBD has not finished");
        t.rig.clock.set_mocktime(i64::from(main[1].time()) + 3_600);
        let relays = vec![t.lock(10, &rival[2])];
        t.ticks(10, HOUR + 600, 600);
        relays
    });
}

#[test]
fn n_enforcement_marks_and_activates() {
    differential(RigConfig::default, |t| {
        let main = super::branch(&t.rig.p, &t.rig.genesis, 3, 15_271);
        let side = super::branch(&t.rig.p, &t.rig.genesis, 2, 15_272);
        let other = super::branch(&t.rig.p, &t.rig.genesis, 1, 15_273);
        for m in main.iter().chain(&side).chain(&other) {
            t.accept(0, m);
        }
        let relays = vec![t.lock(0, &side[1])];
        t.ticks(0, HOUR + 600, 600);
        relays
    });
}

#[test]
fn n_a_lock_on_a_missing_block_then_the_block() {
    differential(RigConfig::default, |t| {
        let main = line(&t.rig, &t.rig.genesis, 2, 15_281);
        let b2 = kid(&t.rig, &main[0], 15_282);
        t.accept(0, &main[0]);
        t.rig.headers(&main[1..]);
        t.accept(0, &b2);
        let relays = vec![t.lock(0, &main[1])];
        t.ticks(0, 2 * HOUR, 600);
        t.accept(2 * HOUR, &main[1]);
        relays
    });
}

#[test]
fn n_spork_off_then_on() {
    differential(RigConfig::default, |t| {
        let main = line(&t.rig, &t.rig.genesis, 1, 15_291);
        let side = line(&t.rig, &t.rig.genesis, 2, 15_292);
        t.accept(0, &main[0]);
        let mut relays = vec![t.lock(0, &main[0])];
        let sporks = t.rig.sporks.as_ref().expect("a fresh rig");
        sporks.set(false);
        intake(&t.rig);
        for m in &side {
            t.accept(0, m);
        }
        t.ticks(0, HOUR + 600, 600);
        sporks.set(true);
        intake(&t.rig);
        relays.push(t.lock(HOUR + 600, &main[0]));
        t.ticks(HOUR + 600, 3 * HOUR, 600);
        relays
    });
}

#[test]
fn n_a_height_mismatch() {
    differential(RigConfig::default, |t| {
        let net = stalled(t, 2, 15_301);
        t.rig.headers(&net[..1]);
        let relays =
            vec![t.lock_sig(0, &lock_at(&net[0], 9)), t.lock_sig(10, &lock_at(&net[1], 4))];
        t.at(600);
        t.rig.headers(&net[1..]);
        t.ticks(600, 2 * HOUR, 600);
        relays
    });
}

/// (n) Runs `scenario`, an existing test, with every V it starts armed and then disarmed (through
/// `TRIPWIRE_DISARMED` on this thread, which spawns them, and overrides
/// `DASHD_TEST_TRIPWIRE_DISABLED`). Its own assertions on Core's state and
/// outputs are the comparison: they must hold either way.
fn differential_existing(name: &str, scenario: fn()) {
    use crate::chain::actor::{TRIPWIRE_DISARMED, TRIPWIRES_DISARMED};

    let disarmed = || TRIPWIRES_DISARMED.with(std::cell::Cell::get);
    let run = |disarm: bool| {
        let before = disarmed();
        TRIPWIRE_DISARMED.with(|d| d.set(Some(disarm)));
        let run = std::panic::catch_unwind(scenario);
        TRIPWIRE_DISARMED.with(|d| d.set(None));
        if let Err(panic) = run {
            std::panic::resume_unwind(panic);
        }
        disarmed() - before
    };
    assert_eq!(run(false), 0, "{name}: a V started disarmed in the armed run");
    assert!(run(true) > 0, "{name}: no V started disarmed");
}

#[test]
fn n_existing_scenarios_run_armed_and_disarmed() {
    use super::super::{evo_chain, evo_chain_cl};

    let scenarios: [(&str, fn()); 9] = [
        (
            "enforcement_marks_the_conflicting_chain_and_activates_the_lock",
            super::enforcement_marks_the_conflicting_chain_and_activates_the_lock,
        ),
        (
            "enforcement_resets_an_invalidated_locked_chain",
            super::enforcement_resets_an_invalidated_locked_chain,
        ),
        (
            "a_lock_on_a_missing_block_notifies_when_it_connects",
            super::a_lock_on_a_missing_block_notifies_when_it_connects,
        ),
        ("a_late_header_moves_the_known_lock", super::a_late_header_moves_the_known_lock),
        (
            "enforcement_is_off_while_the_spork_is_off",
            super::enforcement_is_off_while_the_spork_is_off,
        ),
        (
            "a_known_failed_or_conflicting_header_is_a_duplicate",
            super::a_known_failed_or_conflicting_header_is_a_duplicate,
        ),
        (
            "evo_chain_cl::each_update_point_writes_the_cell",
            evo_chain_cl::each_update_point_writes_the_cell,
        ),
        (
            "evo_chain::chainlock_enforcement_onto_a_fork_across_a_window_synthetic",
            evo_chain::chainlock_enforcement_onto_a_fork_across_a_window_synthetic,
        ),
        (
            "evo_chain::enforcement_resets_a_chain_invalidated_inside_a_window_synthetic",
            evo_chain::enforcement_resets_a_chain_invalidated_inside_a_window_synthetic,
        ),
    ];
    for (name, scenario) in scenarios {
        differential_existing(name, scenario);
    }
}

/// (o) The floor has a header and no data, and an unrelated side-branch block forking below the
/// floor fails ConnectBlock in ABC during that enforcement: no T1, no latch, no alert. Our tip is
/// a1 → m2; the side branch g → s1 → s2 has its data but ties m2's work, so it is not active; the
/// floor is p2 on a1 → p1 → p2, headers only. Enforcement marks m2 conflicting, and ABC then
/// takes the side branch and fails s2 inside that same enforcement.
#[test]
fn o_an_unrelated_failure_is_no_t1() {
    let t = T::new();
    let a1 = kid(&t.rig, &t.rig.genesis, 15_311);
    let m2 = kid(&t.rig, &a1, 15_314);
    t.accept(0, &a1);
    t.accept(0, &m2);
    let path = line(&t.rig, &a1, 2, 15_312);
    let side = line(&t.rig, &t.rig.genesis, 2, 15_313);
    t.fails.always(&side[1]);
    for m in &side {
        t.submit(0, m);
    }
    assert!(t.active(&m2) && !t.active(&side[0]), "the side branch is stored, not active");
    assert_eq!(t.rejection(&side[1]), None, "s2 has not been tried");
    t.rig.headers(&path);
    assert!(t.lock(100, &path[1]));
    assert!(t.rig.status(&m2).has(Status::CONFLICT_CHAINLOCK));
    assert!(t.rig.status(&side[1]).has(Status::FAILED_VALID));
    let id = t.rig.id(&side[1]);
    let entry = t.tw(move |w| w.rejected().get(id).map(|r| (r.stage, r.at_steady)));
    assert_eq!(entry, Some((RejectionStage::Connect, 100)), "s2 failed in that enforcement");
    assert!(!t.active(&path[1]));
    assert!(t.events().is_empty());
    assert_eq!(t.t1_episode(), None);
    t.ticks(100, HOUR - 1, 600);
    assert!(t.events().is_empty());
    assert_eq!(t.latch(), None);
}

/// (p) A restart with a deep stall: V reopened, the monotonic clock back at 0, the stall more
/// than N deep and out of IBD, the lock redelivered at restart and the tip frozen. A2 admits at
/// 600 s backdated to the first observation, and the trip comes at 3,600 s after the restart.
#[test]
fn p_a_restart_inside_a_deep_stall_trips_within_an_hour() {
    let files = SharedFiles::default();
    let mut t = T::with(
        RigConfig {
            files: Some(Box::new(files.clone())),
            ..RigConfig::default()
        },
        true,
    );
    let net = stalled(&t, 100, 15_321);
    // V's mirror starts empty (`cl:37-41`), and the rig's restart keeps the stopped chain, so
    // the stall's lock is first seen at the redelivery.
    t.ticks(0, 1_800, 600);
    let tip = t.with_v(|v| v.tip().expect("a tip"));
    let exit = t.rig.stop();
    let log = Arc::new(ClLog::default());
    let rig = Rig::restart_with(
        exit,
        Box::new(files),
        tip,
        RigConfig {
            after_genesis: Some(3_600),
            ui: Some(log.clone()),
            ..RigConfig::default()
        },
    )
    .expect("V restarts");
    rig.signals.register(log.clone());
    let mono = Arc::new(AtomicU64::new(0));
    let m = mono.clone();
    rig.on_v(move |v| {
        v.tripwire = Tripwire::armed();
        v.tripwire.set_mono(m);
        Ok(())
    });
    intake(&rig);
    t = T {
        rig,
        log,
        mono,
        fails: Fails::default(),
    };
    assert_eq!(t.onset_since(), None, "nothing is persisted");
    assert!(t.lock(0, &net[99]));
    t.tick(0);
    t.ticks(0, TRIPWIRE_DEEP_ADMIT - 1, 60);
    assert_eq!(t.onset_since(), None);
    t.tick(TRIPWIRE_DEEP_ADMIT);
    assert_eq!(t.onset_since(), Some(0));
    t.ticks(TRIPWIRE_DEEP_ADMIT, HOUR - 1, 600);
    assert!(t.events().is_empty());
    t.tick(HOUR);
    assert_eq!(first(&t.events()).kind, DivergenceKind::T2);
}

/// (q) The redelivery match: a restored event, and a lock on its `cl_hash`, then separately its
/// `floor_hash` and its `onset_cl`: one `Redelivered` at intake, none for a second delivery.
#[test]
fn q_a_restored_episode_alerts_once_on_redelivery() {
    for which in 0..3 {
        let t = T::new();
        let net = stalled(&t, 3, 15_331 + which);
        let mut info = live_info(DivergenceKind::T2, net[0].hash());
        info.floor_hash = Some(net[1].hash());
        info.onset_cl = Some(net[2].hash());
        t.restore(info.clone());
        let target = &net[which as usize];
        t.at(10);
        assert!(record(&t.rig, &lock(target)));
        let events = t.events();
        assert!(
            matches!(&events[..], [DivergenceEvent::Redelivered(i)] if i.cl_hash == info.cl_hash && i.restored),
            "{which}: {events:?}"
        );
        settle(&t.rig);
        let again = lock_at(target, target.height as i32 + 1);
        t.lock_sig(20, &again);
        assert!(t.events().is_empty(), "{which}: one per process");
        assert_eq!(t.latch().map(|i| i.restored), Some(true));
    }
}

/// (q2) A T2 episode whose onset lock X differs from its trip-time lock Y and its floor: its
/// `DivergenceInfo` carries `onset_cl` X. Restored, (i) a lock on X redelivers once, and (ii) a
/// later T1 event whose only shared hash is X joins it: no new first alert.
#[test]
fn q2_the_onset_lock_alone_joins_and_redelivers() {
    let t = T::new();
    let net = stalled(&t, 3, 15_341);
    assert!(t.lock(0, &net[0]));
    t.tick(0);
    assert!(t.lock(600, &net[1]));
    t.tick(HOUR);
    let events = t.events();
    let tripped = first(&events).clone();
    assert_eq!((tripped.onset_cl, tripped.cl_hash), (Some(net[0].hash()), net[1].hash()));
    assert_eq!(tripped.floor_hash, None);

    let t = T::new();
    let net = stalled(&t, 3, 15_342);
    let mut info = live_info(DivergenceKind::T2, net[1].hash());
    info.onset_cl = Some(net[0].hash());
    t.restore(info.clone());
    t.at(10);
    assert!(record(&t.rig, &lock(&net[0])));
    assert!(matches!(&t.events()[..], [DivergenceEvent::Redelivered(_)]));

    let t = T::new();
    let main = line(&t.rig, &t.rig.genesis, 4, 15_343);
    t.accept(0, &main[0]);
    t.accept(0, &main[1]);
    let x = &main[2];
    let mut info = live_info(DivergenceKind::T2, main[3].hash());
    info.onset_cl = Some(x.hash());
    t.restore(info.clone());
    t.fails.always(x);
    t.submit(0, x);
    assert!(t.lock(10, x));
    t.ticks(10, 2 * HOUR, 600);
    let events = t.events();
    assert_eq!(firsts(&events), 0, "{events:?}");
    assert!(events.iter().any(|e| matches!(e, DivergenceEvent::Redelivered(_))));
    assert_eq!(t.t1_episode(), Some(info.episode.clone()));
    assert_eq!(t.tw(Tripwire::episode_count), 1);
}

/// (s) Stale entries: each case ends with a lock on X's descendant Y before Y's block arrives
/// (the floor off our chain, X on its path): no T1. Each runs with the connect hook and with it
/// bypassed, so the entry test (1(a)) and the hook (1(b)) each suffice alone.
fn stale(hook: bool, case: impl FnOnce(&T, &[Mined])) {
    let t = T::new();
    t.rig.on_v(move |v| {
        v.tripwire.set_connect_hook(hook);
        Ok(())
    });
    let main = line(&t.rig, &t.rig.genesis, 4, 15_351 + u32::from(hook));
    t.accept(0, &main[0]);
    case(&t, &main);
    let (x, y) = (&main[1], &main[2]);
    assert!(t.active(x));
    assert_eq!(t.rejection(x).is_none(), hook, "the hook removed X's entry");
    t.rig.headers(std::slice::from_ref(y));
    // The later floor is off our chain only because Y's block has not arrived.
    assert!(t.lock(1_000, y));
    t.ticks(1_000, 1_000 + HOUR - 1, 600);
    assert!(t.events().is_empty());
    assert_eq!(t.latch(), None);
}

#[test]
fn s1_a_one_shot_connect_rejection_reversed_by_the_reset() {
    for hook in [true, false] {
        stale(hook, |t, main| {
            let x = &main[1];
            t.fails.once(x);
            t.submit(0, x);
            assert_eq!(t.rejection(x), Some(RejectionStage::Connect));
            assert!(t.lock(10, x));
            assert!(t.events().is_empty());
        });
    }
}

#[cfg(feature = "planted-bugs")]
#[test]
fn s2_a_transient_accept_rejection_that_later_connects() {
    for hook in [true, false] {
        stale(hook, |t, main| {
            let x = &main[1];
            t.rig.v.plant(Some(crate::Planted::RejectAcceptAtHeight(x.height)));
            t.submit(0, x);
            t.rig.v.plant(None);
            assert_eq!(t.rejection(x), Some(RejectionStage::Accept));
            // A lock on X itself would trip T1 here, as in (b): the reset is `reconsiderblock`'s.
            assert_eq!(t.rig.v.reconsider_block(x.hash(), false).recv(), Ok(Ok(())));
            t.accept(20, x);
            assert!(t.events().is_empty());
        });
    }
}

#[test]
fn s3_a_check_stage_entry_then_the_honest_body() {
    for hook in [true, false] {
        stale(hook, |t, main| {
            let x = &main[1];
            t.rig.headers(std::slice::from_ref(x));
            // X's header over a body with a second coinbase, its merkle pass skipped.
            let mut txs = x.block.txs().to_vec();
            txs.push(coinbase(&cb_script(x.height, 15_359)));
            let forged = Arc::new(Block::new(*x.block.header(), txs));
            let r = t.rig.v.submit_block(forged, true, None, true, true).recv();
            assert_eq!(reject_reason(&r.expect("V replies")), "bad-cb-multiple");
            assert_eq!(t.rejection(x), Some(RejectionStage::Check));
            t.accept(20, x);
        });
    }
}

/// (t) The plant's height at V's site: with `RejectAcceptAtHeight(h)` the block at h is rejected
/// with `bad-planted`, and the blocks at h − 1 and h + 1 are not.
#[cfg(feature = "planted-bugs")]
#[test]
fn t_the_accept_plant_rejects_only_its_height() {
    use crate::Planted;

    let t = T::new();
    let main = line(&t.rig, &t.rig.genesis, 3, 15_371);
    let h = main[2].height;
    t.rig.v.plant(Some(Planted::RejectAcceptAtHeight(h)));
    t.accept(0, &main[0]);
    t.accept(0, &main[1]);
    assert_eq!(main[1].height, h - 1);
    assert_eq!(reject_reason(&t.submit(0, &main[2])), "bad-planted");
    t.rig.v.plant(None);
    let sibling = kid(&t.rig, &main[1], 15_372);
    t.accept(0, &sibling);
    t.rig.v.plant(Some(Planted::RejectAcceptAtHeight(h)));
    let above = kid(&t.rig, &sibling, 15_373);
    assert_eq!(above.height, h + 1);
    t.accept(0, &above);
}
