//! `ActivateBestChain` through V (N3): one block per hold in IBD, a reorg in one hold, an invalid
//! block, Core's signal order, publish before notify under §9.4's adversarial schedule (N1 (a)–(d))
//! and SB5's per-step drain inside a hold.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use dash_chain::{FeedSource, Handoff, Needs, Next, Notify, Parker, Refused, Status, TipStep};
use dash_consensus::{Block, BlockId, BlockValidationError, BlockValidationResult, TxRef};
use dash_primitives::BlockHash;

use super::support::{
    Ev, Mined, Rig, RigConfig, batch, branch, cb_script, child, coinbase, mine, regtest,
};
use crate::chain::actor::tests::Stage;
use crate::chain::reply::WaitError;
use crate::chain::{
    BlockConnector, BlockState, BlockStatusWriter, Blocking, ChainHandle, ConnectCx,
    ConnectOutcome, DisconnectPool, DisconnectResult, Fatal, MempoolSink, NoWaitGuard,
    NullConnector, ProposalChecks, RemovalReason, ShutdownFlag, SyncState, UiNotifications,
    VContext, ValidationInterface, ViewTarget, genesis_block, spawn_thread,
};

const SOON: Duration = Duration::from_secs(120);

/// A queued signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Q {
    Tip(BlockId, Option<BlockId>, bool),
    Connected(BlockId),
    Disconnected(BlockId),
}

/// Records the queued signals, and checks in each what a reader of the published state sees
/// (§9.4 (a), and (b)'s `SendMessages` check `active[h(B)] == B`, `np:6425-6427`).
#[derive(Default)]
pub(super) struct Announced {
    v: OnceLock<ChainHandle>,
    log: Mutex<Vec<Q>>,
    /// Callbacks that ran before the published state showed what they announce.
    early: Mutex<Vec<String>>,
}

impl Announced {
    /// Registers after init's signals have run: a late callback announces a tip that is no
    /// longer the published one, which N1 allows.
    pub(super) fn watch(rig: &Rig) -> Arc<Announced> {
        rig.sync();
        let a = Arc::new(Announced::default());
        assert!(a.v.set(rig.v.clone()).is_ok());
        rig.signals.register(a.clone());
        a
    }

    fn see(&self, q: Q) {
        let v = self.v.get().expect("set");
        let (view, snap) = (v.view(), v.reader().snapshot());
        let at = |id: BlockId| {
            let h = v.reader().entry(id).expect("indexed").height;
            (view.snap.active.at(h) == Some(id), snap.active.at(h) == Some(id))
        };
        let shown = match q {
            Q::Tip(new, ..) => {
                view.snap.tip.tip == new && snap.tip.tip == new && at(new) == (true, true)
            }
            Q::Connected(id) => at(id) == (true, true),
            Q::Disconnected(id) => at(id) == (false, false),
        };
        if !shown {
            let early = format!("{q:?} with {:?} published", view.snap.tip.tip);
            self.early.lock().expect("early").push(early);
        }
        self.log.lock().expect("log").push(q);
    }

    pub(super) fn take(&self) -> Vec<Q> {
        assert_eq!(*self.early.lock().expect("early"), Vec::<String>::new(), "N1");
        self.take_unchecked()
    }

    /// The log alone: after a round trip inside one hold (a block disconnected and connected
    /// again) the published state legitimately shows neither step's announcement.
    fn take_unchecked(&self) -> Vec<Q> {
        self.early.lock().expect("early").clear();
        std::mem::take(&mut *self.log.lock().expect("log"))
    }
}

impl ValidationInterface for Announced {
    fn updated_block_tip(&self, new: BlockId, fork: Option<BlockId>, ibd: bool) {
        self.see(Q::Tip(new, fork, ibd));
    }

    fn block_connected(&self, _: &Arc<Block>, id: BlockId) {
        self.see(Q::Connected(id));
    }

    fn block_disconnected(&self, _: &Arc<Block>, id: BlockId) {
        self.see(Q::Disconnected(id));
    }
}

/// What §9.4's adversarial schedule saw.
#[derive(Default)]
pub(super) struct Adversary {
    /// The published tip each release found inside its hold.
    released: Mutex<Vec<BlockId>>,
    /// Every release's stages, in order (N1).
    pub(super) stages: Mutex<Vec<Stage>>,
}

/// §9.4's adversarial schedule: at each hold's release, and again right after its splice, V
/// lets the signals thread run everything queued, so a signal that reached the queue inside the
/// hold, or a splice ahead of the view, would run against the older state.
pub(super) fn adversarial(rig: &Rig) -> Arc<Adversary> {
    let (signals, v) = (rig.signals.clone(), rig.v.clone());
    let adv = Arc::new(Adversary::default());
    let at = adv.clone();
    rig.on_v(move |vv| {
        vv.release_hook = Some(Arc::new(move |stage| {
            if matches!(stage, Stage::Release | Stage::Spliced) {
                signals.sync(&mut Blocking).expect("the signals thread runs");
            }
            if stage == Stage::Release {
                at.released.lock().expect("released").push(v.view().snap.tip.tip);
            }
            at.stages.lock().expect("stages").push(stage);
        }));
        Ok(())
    });
    adv
}

impl Adversary {
    pub(super) fn released(&self) -> Vec<BlockId> {
        std::mem::take(&mut *self.released.lock().expect("released"))
    }
}

/// `uiInterface` with an RPC-shaped reader (§9.4 (c)): each `NotifyBlockTip`, then
/// `getblockcount` from the `ChainView` fast path (`rpc:300-305`, `rpc:431`).
#[derive(Default)]
pub(super) struct Ui {
    pub(super) v: OnceLock<ChainHandle>,
    /// (state, the notified height, `getblockcount` right after).
    pub(super) seen: Mutex<Vec<(SyncState, u32, u32)>>,
}

impl UiNotifications for Ui {
    fn notify_block_tip(&self, state: SyncState, _: BlockHash, height: u32) {
        // Init's ABC notifies before the test holds a handle.
        let Some(v) = self.v.get() else {
            return;
        };
        let count = v.view().snap.tip.tip_height;
        self.seen.lock().expect("seen").push((state, height, count));
    }
}

/// In IBD each outer iteration connects one block: one hold, one release, one
/// `UpdatedBlockTip` each, every signal after its publication; `ProcessNewBlock` replies after
/// its ABC published; the UI and `TipWatch` follow publication.
#[test]
fn ibd_connects_one_block_per_hold_and_announces_after_publication() {
    let ui = Arc::new(Ui::default());
    let rig = Rig::with(RigConfig {
        ui: Some(ui.clone()),
        ..RigConfig::default()
    });
    assert!(ui.v.set(rig.v.clone()).is_ok());
    let adv = adversarial(&rig);
    let a = Announced::watch(&rig);
    let g = rig.id(&rig.genesis);
    assert_eq!(rig.v.tip_watch().get(), Some(rig.genesis.hash()), "init's ABC set it");

    // (c)'s long-poll waiter: `g_best_block` moves only after publication.
    let (watch, v) = (rig.v.tip_watch().clone(), rig.v.clone());
    let seen = Some(rig.genesis.hash());
    let waiter = std::thread::spawn(move || {
        let tip = watch.wait_changed(seen, SOON).expect("unmarked");
        (tip, v.view().snap.tip.tip_height)
    });

    let chain = branch(&rig.p, &rig.genesis, 3, 0);
    for m in &chain {
        rig.accept(m);
        assert_eq!(rig.v.view().snap.tip.tip, rig.id(m), "the reply follows the ABC's publication");
    }
    rig.sync();
    let [x, y, z] = rig.ids(&chain)[..] else {
        unreachable!()
    };
    use Q::*;
    assert_eq!(
        a.take(),
        [
            Connected(x),
            Tip(x, Some(g), true),
            Connected(y),
            Tip(y, Some(x), true),
            Connected(z),
            Tip(z, Some(y), true)
        ]
    );
    assert_eq!(adv.released(), [g, x, y], "the pre-hold tip until each release");
    let hold = [
        Stage::Release,
        Stage::Snapshot,
        Stage::Published,
        Stage::Spliced,
        Stage::Watched,
        Stage::Ui,
    ];
    assert_eq!(*adv.stages.lock().expect("stages"), hold.repeat(3), "N1's order");
    let init = SyncState::InitDownload;
    assert_eq!(*ui.seen.lock().expect("ui"), [(init, 1, 1), (init, 2, 2), (init, 3, 3)]);
    assert_eq!(rig.v.tip_watch().get(), Some(chain[2].hash()));
    // The waiter may wake late, at any of the three; the view it reads is never behind.
    let (tip, published) = waiter.join().expect("the waiter");
    let woke = chain.iter().position(|m| Some(m.hash()) == tip).expect("a new tip") + 1;
    assert!(published as usize >= woke, "TipWatch at {woke} before the view ({published})");
    rig.assert_no_abort();
}

/// A reorg is one hold (Core keeps `cs_main` "until we've made forward progress",
/// `validation.cpp:3775-3776`): readers never see the fork point or a half-connected branch,
/// and the signals come in Core's order: each disconnect at its step, the connects after the
/// step, then one `UpdatedBlockTip` from the fork.
#[test]
fn a_reorg_is_one_hold_and_readers_never_see_the_fork_point() {
    let rig = Rig::new();
    let adv = adversarial(&rig);
    let a = Announced::watch(&rig);
    let main = branch(&rig.p, &rig.genesis, 3, 0);
    for m in &main {
        rig.accept(m);
    }
    rig.sync();
    a.take();
    adv.released();

    let side = branch(&rig.p, &rig.genesis, 4, 1);
    for m in &side {
        rig.accept(m);
    }
    rig.sync();
    let g = rig.id(&rig.genesis);
    let [a1, a2, a3] = rig.ids(&main)[..] else {
        unreachable!()
    };
    let [b1, b2, b3, b4] = rig.ids(&side)[..] else {
        unreachable!()
    };
    use Q::*;
    assert_eq!(
        a.take(),
        [
            Disconnected(a3),
            Disconnected(a2),
            Disconnected(a1),
            Connected(b1),
            Connected(b2),
            Connected(b3),
            Connected(b4),
            Tip(b4, Some(g), true)
        ]
    );
    // B1–B3's ABCs connect nothing (no better chain); B4's holds the whole reorg.
    assert_eq!(adv.released(), [a3; 4]);
    assert_eq!(rig.v.view().snap.tip.tip, b4);
    assert_eq!(rig.idx(|ix| ix.candidates().collect::<Vec<_>>()), [b4]);
}

/// §9.4 (d): an inline listener gets V's `ibd` as an argument, evaluated on V's private tip
/// inside the hold, while the published view still shows the pre-hold tip.
#[test]
fn inline_listeners_get_ibd_from_the_private_tip() {
    #[derive(Default)]
    struct Inline {
        v: OnceLock<ChainHandle>,
        /// (block, `ibd`, the published tip).
        log: Mutex<Vec<(BlockHash, bool, BlockId)>>,
    }
    impl Inline {
        fn push(&self, h: BlockHash, ibd: bool) {
            let published = self.v.get().expect("set").view().snap.tip.tip;
            self.log.lock().expect("log").push((h, ibd, published));
        }
    }
    impl ValidationInterface for Inline {
        fn block_checked(&self, block: &Block, _: &BlockState, ibd: bool) {
            self.push(block.hash(), ibd);
        }
    }

    let rig = Rig::new();
    let inline = Arc::new(Inline::default());
    assert!(inline.v.set(rig.v.clone()).is_ok());
    rig.signals.register(inline.clone());
    // A1 is a day old: the node stays in IBD on it. B1 and B2 are recent.
    let a1 = child(&rig.p, &rig.genesis, 0);
    rig.accept(&a1);
    let now = rig.genesis.time() + 86_400;
    let at = |prev: &Mined, time: u32, salt: u32| {
        let height = prev.height + 1;
        let cb = coinbase(&cb_script(height, salt));
        Mined {
            block: mine(&rig.p, prev.hash(), time, vec![cb]),
            height,
        }
    };
    let b1 = at(&rig.genesis, now - 600, 1);
    let b2 = at(&b1, now - 300, 1);
    rig.accept(&b1);
    inline.log.lock().expect("log").clear();
    rig.accept(&b2);
    let published = rig.id(&a1);
    // B1's check runs with genesis as the private tip (IBD); B2's with B1 (out of IBD), while
    // readers still see A1.
    assert_eq!(
        *inline.log.lock().expect("log"),
        [(b1.hash(), true, published), (b2.hash(), false, published)]
    );
}

/// `ConnectBlock` failing the listed blocks (`bad-cb-amount`); the rest connect.
pub(super) struct Rejects(pub(super) HashSet<BlockHash>);

impl BlockConnector for Rejects {
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
        if self.0.contains(&block.hash()) {
            let err = BlockValidationError::new(BlockValidationResult::Consensus, "bad-cb-amount");
            return Ok(ConnectOutcome::Invalid(err));
        }
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

/// B2 fails at connect inside a reorg from A1: `InvalidBlockFound(B2)`, `InvalidChainFound` of
/// the batch's top B3 (Core's `front()`, `validation.cpp:3654-3656`), B3 marked `FAILED_CHILD`
/// by `FindMostWorkChain`, and the same hold returns to A1. No `UpdatedBlockTip`: the tip ends
/// where it started (`pindexFork == pindexNewTip`).
#[test]
fn an_invalid_block_fails_its_chain_and_the_hold_returns_to_the_best_valid() {
    let p = regtest();
    let genesis = Mined {
        block: genesis_block(&p),
        height: 0,
    };
    let a1 = child(&p, &genesis, 0);
    let side = branch(&p, &genesis, 3, 1);
    let rig = Rig::with(RigConfig {
        connector: Some(Box::new(Rejects(HashSet::from([side[1].hash()])))),
        ..RigConfig::default()
    });
    let adv = adversarial(&rig);
    let a = Announced::watch(&rig);
    rig.accept(&a1);
    rig.sync();
    a.take();
    rig.headers(&side);
    // B3 waits unlinked for B2's data; B2 then makes B3 the most-work candidate.
    rig.accept(&side[0]);
    rig.accept(&side[2]);
    rig.rec.take();
    adv.released();
    rig.accept(&side[1]);
    rig.sync();
    let a1i = rig.id(&a1);
    let [b1, _, b3] = rig.ids(&side)[..] else {
        unreachable!()
    };
    use Q::*;
    assert_eq!(
        a.take_unchecked(),
        [Disconnected(a1i), Connected(b1), Disconnected(b1), Connected(a1i)]
    );
    assert_eq!(rig.v.view().snap.tip.tip, a1i);
    assert_eq!(adv.released(), [a1i], "one hold, from A1 back to A1");
    assert!(rig.status(&side[1]).has(Status::FAILED_VALID));
    assert!(rig.status(&side[2]).has(Status::FAILED_CHILD));
    assert!(!rig.status(&side[0]).has(Status::FAILED_MASK));
    assert_eq!(rig.idx(|ix| ix.best_invalid()), Some(b3), "the batch's top");
    let checked: Vec<_> =
        rig.rec.take().into_iter().filter(|e| matches!(e, Ev::Checked(..))).collect();
    let bad = BlockState::Invalid(BlockValidationError::new(
        BlockValidationResult::Consensus,
        "bad-cb-amount",
    ));
    assert_eq!(
        checked,
        [
            Ev::Checked(side[0].hash(), BlockState::Valid),
            Ev::Checked(side[1].hash(), bad),
            Ev::Checked(a1.hash(), BlockState::Valid)
        ]
    );
    rig.assert_no_abort();
}

/// Fails one block's connect: with a system error, `state.Error` after an `AbortNode` inside
/// `ConnectBlock` (`Ok(SystemError)`), or with a storage fault, Core's `std::abort()` inside it
/// (`Err`, CH3 §2.1).
pub(super) struct Breaks {
    pub(super) at: BlockHash,
    pub(super) abort: bool,
}

impl BlockConnector for Breaks {
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
        match (block.hash() == self.at, self.abort) {
            (true, true) => Err(Fatal::new("disk on fire")),
            (true, false) => Ok(ConnectOutcome::SystemError("disk on fire".into())),
            (false, _) => NullConnector.connect_block(cx, v, target, block, checked, id, st),
        }
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

/// The mempool calls `ActivateBestChainStep` makes around its reorg.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Pool {
    New,
    Update(bool),
    Check(u32),
}

pub(super) struct Mempool(pub(super) Arc<Mutex<Vec<Pool>>>);

impl Mempool {
    fn log(&self, p: Pool) {
        self.0.lock().expect("pool").push(p);
    }
}

impl MempoolSink for Mempool {
    fn new_disconnect_pool(&mut self) -> Box<dyn DisconnectPool> {
        self.log(Pool::New);
        Box::new(())
    }

    fn maybe_update_for_reorg(&mut self, _: Box<dyn DisconnectPool>, add: bool) {
        self.log(Pool::Update(add));
    }

    fn remove_for_block(&mut self, _: &[TxRef], _: u32) {}

    fn remove_expired_asset_unlock(&mut self, _: u32) {}

    fn remove_recursive(&mut self, _: &TxRef, _: RemovalReason) {}

    fn add_transactions_updated(&mut self, _: u32) {}

    fn check(&mut self, height: u32) {
        self.log(Pool::Check(height));
    }
}

/// A system error mid-reorg is `AbortNode` after `MaybeUpdateMempoolForReorg(disconnectpool,
/// false)` (`validation.cpp:3662-3667`): the mempool matches the tip "in case any observers try
/// to use it before shutdown". A storage fault, where Core's process has aborted, takes the same
/// way out in V: CH1-node's, unobservable as the node stops.
#[test]
fn a_system_error_mid_reorg_updates_the_mempool_then_aborts() {
    for abort in [false, true] {
        let p = regtest();
        let genesis = Mined {
            block: genesis_block(&p),
            height: 0,
        };
        let a1 = child(&p, &genesis, 0);
        let side = branch(&p, &genesis, 2, 1);
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut rig = Rig::with(RigConfig {
            connector: Some(Box::new(Breaks {
                at: side[1].hash(),
                abort,
            })),
            mempool: Some(Box::new(Mempool(log.clone()))),
            ..RigConfig::default()
        });
        rig.accept(&a1);
        rig.accept(&side[0]);
        let steps = [Pool::New, Pool::Check(1), Pool::New, Pool::Check(2)];
        assert_eq!(*log.lock().expect("pool"), steps, "init's genesis step, A1's; B1 none");
        log.lock().expect("pool").clear();
        let feeds = rig.v.query(|q| q.chain.feeds()).recv().expect("V");
        rig.rec.take();

        let r = rig.v.process_block(side[1].block.clone(), true, None, true).recv();
        assert_eq!(r, Err(WaitError::Disconnected), "abort {abort}");
        // `BlockChecked` follows `ConnectBlock`'s return, a system error's included (:3437-3438),
        // but not a storage fault's: Core aborts inside `ConnectBlock` (CH3 §2.1).
        let checked: Vec<Ev> =
            rig.rec.take().into_iter().filter(|e| matches!(e, Ev::Checked(..))).collect();
        let mut want = vec![Ev::Checked(side[0].hash(), BlockState::Valid)];
        if !abort {
            want.push(Ev::Checked(side[1].hash(), BlockState::Error("disk on fire".into())));
        }
        assert_eq!(checked, want, "abort {abort}");
        // The abort left `cs_main`'s scope (:3788-3791): the tip lock is free.
        let deadline = std::time::Instant::now() + SOON;
        let registered = loop {
            match feeds.register(Needs::default()) {
                Err(Refused::HoldActive) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(1));
                }
                r => break r,
            }
        };
        let Ok(id) = registered else {
            panic!("abort {abort}: {registered:?}");
        };
        feeds.deregister(id);
        let exit = rig.stop();
        assert!(exit.fatal.is_some_and(|f| f.0.contains("disk on fire")), "abort {abort}");
        assert_eq!(*log.lock().expect("pool"), [Pool::New, Pool::Update(false)], "abort {abort}");
    }
}

/// SB5's writer rule inside a hold (§2.5): at Q = 1 a marked subscriber pops each step's event
/// while the hold is still open, so V drained it after that step and before the next one, and
/// the reorg completes.
#[test]
fn a_slow_subscriber_is_drained_after_every_step_inside_the_hold() {
    let rig = Rig::with(RigConfig {
        feed_capacity: Some(1),
        ..RigConfig::default()
    });
    let main = branch(&rig.p, &rig.genesis, 3, 0);
    for m in &main {
        rig.accept(m);
    }
    let side = branch(&rig.p, &rig.genesis, 4, 1);
    for m in &side[..3] {
        rig.accept(m);
    }
    let feeds = rig.v.query(|q| q.chain.feeds()).recv().expect("V");
    let id = feeds.register(Needs::default()).expect("registers");
    let tip = FeedSource::<()>::snapshot(&feeds).tip();
    let tip_seq = FeedSource::<()>::snapshot(&feeds).tip_seq();
    let Ok(Handoff::Live {
        mut feed,
        ..
    }) = feeds.subscribe_at(id, &tip, &mut || Ok::<(), ()>(()))
    else {
        panic!("subscribe_at")
    };
    let (reader, a3) = (rig.v.reader().clone(), rig.id(&main[2]));
    let s = spawn_thread("s", move || {
        let _marked = NoWaitGuard::set();
        let p = Parker::new();
        feed.set_waker(Notify::Thread(p.clone()));
        let mut got = Vec::new();
        while got.len() < 7 {
            match feed.next() {
                Next::Event(_) => {}
                Next::Empty => {
                    assert!(p.park_timeout(SOON), "V never stepped");
                    continue;
                }
                Next::Control(_) => continue,
            }
            if got.len() < 6 {
                // V cannot take the last step until this pop: the hold is open.
                assert_eq!(reader.snapshot().tip.tip, a3, "no release yet");
            }
            std::thread::sleep(Duration::from_millis(5));
            got.push(feed.pop().expect("selected"));
        }
        got
    })
    .expect("spawn S");
    rig.accept(&side[3]);
    let got = s.join().expect("unmarked joiner").expect("S");
    let ids: Vec<_> = got
        .iter()
        .map(|e| match e {
            TipStep::Disconnected {
                block,
                ..
            } => (false, block.id),
            TipStep::Connected {
                block,
                ..
            } => (true, block.id),
        })
        .collect();
    let want: Vec<_> = main
        .iter()
        .rev()
        .map(|m| (false, rig.id(m)))
        .chain(side.iter().map(|m| (true, rig.id(m))))
        .collect();
    assert_eq!(ids, want);
    let seqs: Vec<u64> = got.iter().map(TipStep::seq).collect();
    assert_eq!(seqs, (tip_seq + 1..=tip_seq + 7).collect::<Vec<_>>(), "one seq per step");
    assert_eq!(rig.v.view().snap.tip.tip, rig.id(&side[3]));
    rig.assert_no_abort();
    feeds.deregister(id);
}

/// `-stopatheight` (`validation.cpp:3836`): `StartShutdown()` once the new tip reaches it. The
/// argument narrows to Core's `int` (`:3758`), so 2^32 + 2 is 2.
#[test]
fn stop_at_height_requests_shutdown() {
    for stop_at_height in [2, (1 << 32) + 2] {
        let rig = Rig::with(RigConfig {
            stop_at_height,
            ..RigConfig::default()
        });
        let chain = branch(&rig.p, &rig.genesis, 2, 0);
        rig.accept(&chain[0]);
        assert!(!rig.v.shutdown_requested(), "{stop_at_height}");
        rig.accept(&chain[1]);
        assert!(rig.v.shutdown_requested(), "{stop_at_height}");
    }
}

/// `-stopatheight=-1` (ND1 2c Q-A): Core's test is `n != 0 && height >= n`, so init's ABC
/// connecting genesis (height 0) requests shutdown, as in dashd.
#[test]
fn stop_at_height_minus_one_stops_at_genesis() {
    let rig = Rig::with(RigConfig {
        stop_at_height: -1,
        ..RigConfig::default()
    });
    assert!(rig.shutdown.shutdown_requested());
    assert_eq!(rig.v.view().snap.tip.tip_height, 0);
}

/// `-stopatheight=0` is off: no tip requests shutdown.
#[test]
fn stop_at_height_zero_never_stops() {
    let rig = Rig::with(RigConfig {
        stop_at_height: 0,
        ..RigConfig::default()
    });
    for m in &branch(&rig.p, &rig.genesis, 2, 0) {
        rig.accept(m);
    }
    assert!(!rig.shutdown.shutdown_requested());
    assert_eq!(rig.v.view().snap.tip.tip_height, 2);
}

/// D52: `-stopatheight` is the node's `StartShutdown()` (`validation.cpp:3836`), and ABC stops
/// at its next `ShutdownRequested()` (`:3847`): a block above it stays unconnected.
#[test]
fn stop_at_height_stops_abc_at_that_height() {
    let rig = Rig::with(RigConfig {
        stop_at_height: 2,
        ..RigConfig::default()
    });
    let chain = branch(&rig.p, &rig.genesis, 3, 0);
    rig.headers(&chain);
    rig.accept(&chain[2]);
    rig.accept(&chain[1]);
    // One ABC with three blocks to connect: one step each.
    rig.accept(&chain[0]);
    assert!(rig.shutdown.shutdown_requested());
    assert_eq!(rig.v.view().snap.tip.tip_height, 2);
}

/// D52: the node's shutdown (a signal, the `stop` RPC, the disk check) reaches ABC at its next
/// `ShutdownRequested()` (`v:3847`): one step connects, then ABC stops.
#[test]
fn the_nodes_shutdown_stops_abc_after_one_step() {
    let rig = Rig::new();
    let chain = branch(&rig.p, &rig.genesis, 3, 0);
    rig.headers(&chain);
    rig.accept(&chain[2]);
    rig.accept(&chain[1]);
    rig.shutdown.start_shutdown();
    rig.accept(&chain[0]);
    assert_eq!(rig.v.view().snap.tip.tip_height, 1);
}

/// D52: InvalidateBlock's loop checks the node's flag first (`v:3943`): with it set, nothing is
/// disconnected.
#[test]
fn the_nodes_shutdown_stops_invalidate_block_before_a_disconnect() {
    let rig = Rig::new();
    let chain = branch(&rig.p, &rig.genesis, 2, 0);
    for m in &chain {
        rig.accept(m);
    }
    rig.shutdown.start_shutdown();
    let _ = rig.v.invalidate_block(chain[0].hash()).recv().expect("V replies");
    assert_eq!(rig.v.view().snap.tip.tip_height, 2);
}

/// Headers alone change nothing: an explicit ABC with no better candidate connects nothing.
#[test]
fn an_explicit_abc_without_blocks_is_a_no_op() {
    let rig = Rig::new();
    let chain = branch(&rig.p, &rig.genesis, 2, 0);
    rig.v.process_headers(batch(&chain), None).recv().expect("V");
    rig.v.activate_best_chain().recv().expect("V").expect("the ABC");
    assert_eq!(rig.v.view().snap.tip.tip, rig.id(&rig.genesis));
}
