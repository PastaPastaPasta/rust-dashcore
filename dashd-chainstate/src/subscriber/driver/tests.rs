//! The driver over the scripted producer and the toy (§2.9, §4.4.1): catch-up and the handoff,
//! rule 1's two cases, reorgs above and below L, the gate, the limit, and a power loss after every
//! step.

use std::sync::Arc;
use std::time::Duration;

use dash_chain::BlockRef;

use super::*;
use crate::store::{GlobalFailStop, ProbeEngine, RecordingSink};
use crate::subscriber::{
    NoBlocks, ScriptedFeeds, TOY_ARCHIVE, TOY_DB, TOY_KEYED, ToyIndex, TreeIndex, toy_archived,
    toy_check_at_l, toy_expected, toy_rows_above,
};

type Toy = SubscriberDriver<ToyIndex>;

struct Fx {
    tree: TreeIndex,
    feeds: ScriptedFeeds,
    hub: Arc<SubscriberHub>,
    main: Vec<BlockRef>,
    db: Arc<GuardedEngine<ProbeEngine>>,
}

fn guarded(e: ProbeEngine) -> Arc<GuardedEngine<ProbeEngine>> {
    Arc::new(GuardedEngine::new(e, GlobalFailStop::new(), Arc::new(RecordingSink::default())))
}

/// Genesis plus `n` blocks on V's chain, Q = 4.
fn fx(n: usize) -> Fx {
    let tree = TreeIndex::new();
    let g = tree.genesis();
    let main = std::iter::once(g).chain(tree.branch(g, n)).collect::<Vec<_>>();
    let feeds = ScriptedFeeds::new(4, main.clone());
    let hub = Arc::new(SubscriberHub::with_feeds(Arc::new(feeds.clone()), Arc::new(tree.clone())));
    Fx {
        tree,
        feeds,
        hub,
        main,
        db: guarded(ProbeEngine::new(&TOY_DB)),
    }
}

fn cfg() -> DriverConfig {
    DriverConfig {
        journal_budget: 1 << 20,
        unbounded_delete_span: 64,
        d_max: None,
        cadence: 1000,
    }
}

/// A budget below one entry: each entry goes alone into an empty journal.
fn tiny() -> DriverConfig {
    DriverConfig {
        journal_budget: 1,
        ..cfg()
    }
}

impl Fx {
    fn open(&self, cfg: DriverConfig) -> Toy {
        SubscriberDriver::open(
            ToyIndex::mandatory(),
            Arc::clone(&self.db),
            Arc::clone(&self.hub),
            Arc::new(NoBlocks),
            cfg,
        )
        .expect("opens")
    }

    /// An optional subscriber on the shared database (Core's lazy rewind, D38, D45, D46).
    fn open_optional(&self, cfg: DriverConfig) -> Toy {
        SubscriberDriver::open(
            ToyIndex::with(false, 0),
            Arc::clone(&self.db),
            Arc::clone(&self.hub),
            Arc::new(NoBlocks),
            cfg,
        )
        .expect("opens")
    }

    /// W's step 5 at V's tip.
    fn flush(&self, flush: u64) -> Vec<SubscriberId> {
        let tip = self.feeds.tip().expect("a tip");
        self.hub.publish_durable(flush, tip, self.feeds.tip_seq())
    }

    fn path(&self, tip: BlockRef) -> Vec<BlockRef> {
        self.tree.path(tip)
    }

    /// The database at L, as a power loss now leaves it and the next open finds it. The loss
    /// is the shared engine's, so it ends the run.
    fn check_after_power_loss(self, d: Toy) {
        let restarted = guarded(self.db.inner().restart());
        let feeds = ScriptedFeeds::new(4, self.path(self.feeds.tip().expect("a tip")));
        let hub = Arc::new(SubscriberHub::with_feeds(Arc::new(feeds), Arc::new(self.tree.clone())));
        let reopened = SubscriberDriver::open(
            ToyIndex::mandatory(),
            Arc::clone(&restarted),
            hub,
            Arc::new(NoBlocks),
            cfg(),
        )
        .expect("reopens");
        assert_eq!(reopened.durable(), d.durable());
        if let Some(l) = d.durable() {
            toy_check_at_l(&*restarted, &self.path(l)).expect("the state at L");
        }
        let l = d.durable().map_or(-1, |l| i64::from(l.height));
        let above = toy_rows_above(&*restarted, 0);
        assert!(above.iter().all(|&h| i64::from(h) <= l), "the blind rewind: {above:?} above {l}");
    }
}

fn sync(d: &mut Toy) {
    for _ in 0..1000 {
        if d.sync_step().expect("syncs") == SyncProgress::Live {
            return;
        }
    }
    panic!("never live");
}

/// Steps until idle or parked.
fn drain(d: &mut Toy) -> SubStep {
    for _ in 0..1000 {
        match d.step().expect("steps") {
            SubStep::Progress => {}
            s => return s,
        }
    }
    panic!("never idle");
}

/// The keyed rows a reader sees at p (O-R2-M1: the journal over the DB).
fn assert_reads_at(d: &Toy, path: &[BlockRef]) {
    let (want, _) = toy_expected(path);
    for (cf, k, v) in want.iter().filter(|r| r.0 == TOY_KEYED) {
        assert_eq!(d.read(*cf, k).expect("reads").as_deref(), Some(&v[..]), "keyed {k:?}");
    }
    assert_eq!(d.best_block(), path.last().copied());
}

#[test]
fn catch_up_commits_at_t_ack_and_hands_off_at_the_tip() {
    let fx = fx(10);
    fx.hub.publish_durable(1, fx.main[6], 0);
    let mut d = fx.open(cfg());
    assert_eq!(d.class(), SubscriberClass::Syncing);
    sync(&mut d);
    assert_eq!(d.class(), SubscriberClass::Synced);
    assert_eq!(d.progress(), Some(fx.main[10]));
    // R11a: capped at T_ack, not at the tip it handed off at.
    assert_eq!(d.durable(), Some(fx.main[6]));
    toy_check_at_l(&*fx.db, &fx.main[..=6]).expect("the state at L");
    assert_reads_at(&d, &fx.main);
    fx.check_after_power_loss(d);
}

/// Rule 1, case A: every event up to `tip_seq` processed, so the commit is at `LCA(p, new)`.
#[test]
fn case_a_commits_at_new_and_acks() {
    let fx = fx(4);
    fx.flush(1);
    let mut d = fx.open(cfg());
    sync(&mut d);
    for b in fx.tree.branch(fx.main[4], 3) {
        fx.feeds.connect(b);
    }
    assert_eq!(drain(&mut d), SubStep::Idle);
    assert_eq!(fx.flush(2), vec![d.id()]);
    assert_eq!(drain(&mut d), SubStep::Idle);
    let new = fx.feeds.tip().expect("a tip");
    assert!(fx.hub.acks_done(2));
    assert_eq!(d.durable(), Some(new));
    assert_eq!(fx.hub.durable_locator(d.id()), Some(new));
    toy_check_at_l(&*fx.db, &fx.path(new)).expect("the state at L");
    fx.check_after_power_loss(d);
}

/// Rule 1, case B: the commit arrives before its tip event; the driver processes up to it first.
#[test]
fn case_b_processes_up_to_the_tip_event_then_commits() {
    let fx = fx(4);
    fx.flush(1);
    let mut d = fx.open(cfg());
    sync(&mut d);
    for b in fx.tree.branch(fx.main[4], 3) {
        fx.feeds.connect(b);
    }
    fx.flush(2);
    let new = fx.feeds.tip().expect("a tip");
    // An event after `tip_seq` is not part of the commit.
    let after = fx.tree.child(new);
    fx.feeds.connect(after);
    assert_eq!(drain(&mut d), SubStep::Idle);
    assert!(fx.hub.acks_done(2));
    assert_eq!(d.durable(), Some(new));
    assert_eq!(d.progress(), Some(after));
    assert_reads_at(&d, &fx.path(after));
}

/// §2.9 item 4 and rule 6: a reorg above L archives the disconnected blocks after `sync_files`,
/// and the reads at p merge the journal over the DB.
#[test]
fn a_reorg_above_l_archives_and_reads_merge_the_journal() {
    let fx = fx(10);
    fx.flush(1);
    let mut d = fx.open(cfg());
    sync(&mut d);
    let stale = fx.tree.branch(fx.main[10], 2);
    for &b in &stale {
        fx.feeds.connect(b);
    }
    assert_eq!(drain(&mut d), SubStep::Idle);
    fx.feeds.disconnect();
    fx.feeds.disconnect();
    let side = fx.tree.branch(fx.main[10], 3);
    for &b in &side {
        fx.feeds.connect(b);
    }
    assert_eq!(drain(&mut d), SubStep::Idle);
    assert_eq!(d.durable(), Some(fx.main[10]), "nothing above L was committed");
    assert_reads_at(&d, &fx.path(side[2]));
    for &b in &stale {
        let (_, s) = toy_expected(&fx.path(b));
        let (k, v) = toy_archived(&b, &s);
        assert_eq!(fx.db.read(TOY_ARCHIVE, &k).expect("reads"), Some(v), "archived {}", b.height);
    }
    fx.flush(2);
    drain(&mut d);
    toy_check_at_l(&*fx.db, &fx.path(side[2])).expect("the state at L");
    fx.check_after_power_loss(d);
}

/// §2.9 item 5c: a disconnect below L makes the rewind commit due; it lands before the next
/// connect, archiving (F, L] from barrier-covered reads.
#[test]
fn a_disconnect_below_l_rewinds_before_the_next_connect() {
    let fx = fx(10);
    fx.flush(1);
    let mut d = fx.open(cfg());
    sync(&mut d);
    assert_eq!(d.durable(), Some(fx.main[10]));
    fx.feeds.disconnect();
    fx.feeds.disconnect();
    let side = fx.tree.branch(fx.main[8], 3);
    for &b in &side {
        fx.feeds.connect(b);
    }
    assert_eq!(drain(&mut d), SubStep::Idle);
    assert_eq!(d.durable(), Some(fx.main[8]), "F");
    for b in [fx.main[9], fx.main[10]] {
        let (_, s) = toy_expected(&fx.path(b));
        let (k, v) = toy_archived(&b, &s);
        assert_eq!(fx.db.read(TOY_ARCHIVE, &k).expect("reads"), Some(v));
    }
    assert_reads_at(&d, &fx.path(side[2]));
    fx.flush(2);
    drain(&mut d);
    toy_check_at_l(&*fx.db, &fx.path(side[2])).expect("the state at L");
    fx.check_after_power_loss(d);
}

/// Rule 4 for a Synced target: it drains its disconnects and reports once L is not above `to`.
#[test]
fn the_gate_rewinds_a_synced_target() {
    let fx = fx(10);
    fx.flush(1);
    let mut d = fx.open(cfg());
    sync(&mut d);
    fx.feeds.disconnect();
    fx.feeds.disconnect();
    let to = fx.main[8];
    assert_eq!(fx.hub.open_rewind_gate(2, to), vec![d.id()]);
    assert_eq!(drain(&mut d), SubStep::Idle);
    assert!(fx.hub.gate_done(2));
    assert_eq!(d.durable(), Some(to));
    toy_check_at_l(&*fx.db, &fx.main[..=8]).expect("the state at L");
    fx.check_after_power_loss(d);
}

/// Rule 4 for a Syncing target: it rewinds at once.
#[test]
fn the_gate_rewinds_a_syncing_target_at_once() {
    let fx = fx(10);
    fx.flush(1);
    let mut d = fx.open(cfg());
    sync(&mut d);
    let id = d.id();
    d.stop().expect("stops");
    // Reopened Syncing, with V reorganised below its L.
    fx.feeds.disconnect();
    fx.feeds.disconnect();
    let mut d = fx.open(cfg());
    assert_eq!(d.durable(), Some(fx.main[10]));
    assert_ne!(d.id(), id);
    let to = fx.main[8];
    assert_eq!(fx.hub.open_rewind_gate(2, to), vec![d.id()]);
    while !fx.hub.gate_done(2) {
        d.sync_step().expect("syncs");
    }
    assert_eq!(d.durable(), Some(to));
    toy_check_at_l(&*fx.db, &fx.main[..=8]).expect("the state at L");
}

/// H6: a live subscriber's path from event to ack takes no tip lock: it acks while V holds it.
#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "a plain test thread standing in for the events thread, and a bounded wait for it"
)]
fn a_live_subscriber_acks_while_v_holds_the_tip() {
    let fx = fx(4);
    fx.flush(1);
    let mut d = fx.open(cfg());
    sync(&mut d);
    for b in fx.tree.branch(fx.main[4], 2) {
        fx.feeds.connect(b);
    }
    fx.flush(2);
    let held = fx.feeds.hold_tip();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        tx.send(drain(&mut d)).expect("sends");
    });
    let step = rx.recv_timeout(std::time::Duration::from_secs(60));
    drop(held);
    assert_eq!(step, Ok(SubStep::Idle), "the driver waited on the tip lock");
    assert!(fx.hub.acks_done(2));
}

/// Rule 3: at the limit with nothing to compress, the driver parks and posts `FlushDue` once; a
/// `FlushCommit` then checkpoints it forward (case B at the limit).
#[test]
fn the_limit_parks_with_flush_due_and_a_commit_frees_it() {
    let fx = fx(2);
    fx.flush(1);
    let mut d = fx.open(tiny());
    sync(&mut d);
    for b in fx.tree.branch(fx.main[2], 3) {
        fx.feeds.connect(b);
    }
    assert_eq!(drain(&mut d), SubStep::Parked);
    assert_eq!(d.journal_len(), 1);
    assert!(fx.hub.take_flush_due());
    assert_eq!(drain(&mut d), SubStep::Parked);
    assert!(!fx.hub.flush_due(), "posted once per park");
    fx.flush(2);
    assert_eq!(drain(&mut d), SubStep::Idle);
    assert!(fx.hub.acks_done(2));
    let tip = fx.feeds.tip().expect("a tip");
    assert_eq!(d.durable(), Some(tip));
    toy_check_at_l(&*fx.db, &fx.path(tip)).expect("the state at L");
}

/// Rule 3: a round trip queued behind the limit is compressed, and its `seq`s are skipped.
#[test]
fn a_round_trip_at_the_limit_is_compressed() {
    let fx = fx(2);
    fx.flush(1);
    let mut d = fx.open(tiny());
    sync(&mut d);
    let a = fx.tree.child(fx.main[2]);
    fx.feeds.connect(a);
    let b = fx.tree.child(a);
    fx.feeds.connect(b);
    fx.feeds.disconnect();
    let c = fx.tree.child(a);
    fx.feeds.connect(c);
    assert_eq!(drain(&mut d), SubStep::Parked);
    assert_eq!(d.progress(), Some(a));
    assert_eq!(fx.feeds.queued(d.id()), 1, "b's round trip is gone; c waits");
    fx.flush(2);
    assert_eq!(drain(&mut d), SubStep::Idle);
    assert_eq!(d.durable(), Some(c));
    toy_check_at_l(&*fx.db, &fx.path(c)).expect("the state at L");
}

/// A Syncing subscriber at its limit commits up to T_ack, then blocks with `FlushDue` until
/// T_ack moves.
#[test]
fn catch_up_at_the_limit_blocks_until_t_ack_moves() {
    let fx = fx(10);
    fx.hub.publish_durable(1, fx.main[3], 0);
    let mut d = fx.open(tiny());
    let mut blocked = false;
    for _ in 0..100 {
        if d.sync_step().expect("syncs") == SyncProgress::Blocked {
            blocked = true;
            break;
        }
    }
    assert!(blocked);
    assert_eq!(d.durable(), Some(fx.main[3]));
    assert!(fx.hub.take_flush_due());
    fx.flush(2);
    sync(&mut d);
    assert_eq!(d.durable(), Some(fx.main[10]));
    toy_check_at_l(&*fx.db, &fx.main).expect("the state at L");
}

/// O-R2-M2: the high-water mark is monotone across rewinds and restarts, and floored by the
/// files' length at open.
#[test]
fn the_high_water_mark_is_monotone() {
    let fx = fx(10);
    fx.flush(1);
    let mut d = fx.open(cfg());
    sync(&mut d);
    for _ in 0..4 {
        fx.feeds.disconnect();
    }
    let side = fx.tree.branch(fx.main[6], 1);
    fx.feeds.connect(side[0]);
    drain(&mut d);
    fx.flush(2);
    drain(&mut d);
    let s = toy_check_at_l(&*fx.db, &fx.path(side[0])).expect("the state at L");
    assert_eq!((s.count, s.high_water), (8, 11));
    d.stop().expect("stops");
    let floored = SubscriberDriver::open(
        ToyIndex::with(true, 40),
        Arc::clone(&fx.db),
        Arc::clone(&fx.hub),
        Arc::new(NoBlocks),
        cfg(),
    )
    .expect("opens");
    assert_eq!(floored.durable_state.high_water, 40);
}

/// The power-loss script: catch-up with cadence commits, then connects, a commit, a reorg below
/// L and another commit. Each driver step spends one of `budget`; true if the script finished.
fn script(fx: &Fx, d: &mut Toy, mut budget: usize) -> bool {
    let mut spend = || {
        let ok = budget > 0;
        budget = budget.saturating_sub(1);
        ok
    };
    loop {
        if !spend() {
            return false;
        }
        match d.sync_step().expect("syncs") {
            SyncProgress::Live => break,
            // W answers `FlushDue` with a flush at V's tip.
            SyncProgress::Blocked => {
                fx.flush(1);
            }
            SyncProgress::Progress => {}
        }
    }
    let ops: [&dyn Fn(&Fx); 5] = [
        &|fx| {
            let t = fx.feeds.tip().expect("a tip");
            fx.feeds.connect(fx.tree.child(t));
        },
        &|fx| {
            fx.flush(2);
        },
        &|fx| {
            for _ in 0..3 {
                fx.feeds.disconnect();
            }
        },
        &|fx| {
            let t = fx.feeds.tip().expect("a tip");
            for b in fx.tree.branch(t, 3) {
                fx.feeds.connect(b);
            }
        },
        &|fx| {
            fx.flush(3);
        },
    ];
    for op in ops {
        op(fx);
        loop {
            if !spend() {
                return false;
            }
            if d.step().expect("steps") != SubStep::Progress {
                break;
            }
        }
    }
    true
}

/// A power loss after any step reopens at the last durable L with the state at L (§2.9 item 7).
#[test]
fn a_power_loss_after_every_step_reopens_at_l() {
    for steps in 0.. {
        let fx = fx(6);
        fx.hub.publish_durable(1, fx.main[4], 0);
        let mut d = fx.open(DriverConfig {
            cadence: 2,
            ..tiny()
        });
        let done = script(&fx, &mut d, steps);
        if done {
            assert!(fx.hub.acks_done(3));
            assert_eq!(d.durable(), fx.feeds.tip());
        }
        fx.check_after_power_loss(d);
        if done {
            return;
        }
        assert!(steps < 500, "the script ends");
    }
}

/// Core's prune check at `BaseIndex::Init` (`index/base.cpp:90-122`): the data from V's tip down
/// to L, or to the fork for an L off V's chain, or below the tip to genesis with no locator.
#[test]
fn init_prune_check() {
    let fx = fx(10);
    let side = fx.tree.branch(fx.main[4], 3);
    let check = |l: Option<BlockRef>| prune_violation(&*fx.feeds.snapshot(), &fx.tree, l);
    for l in [None, Some(fx.main[3]), Some(fx.main[10]), Some(side[1])] {
        assert!(!check(l), "nothing pruned, L {l:?}");
    }
    // Below L, and on a side branch above the fork: not needed.
    fx.tree.prune(fx.main[2]);
    fx.tree.prune(side[0]);
    assert!(!check(Some(fx.main[3])));
    assert!(!check(Some(side[2])));
    // A synced index needs nothing; one behind needs every block from the tip to L, L included,
    // or to the fork.
    fx.tree.prune(fx.main[4]);
    assert!(!check(Some(fx.main[10])));
    assert!(check(Some(fx.main[3])));
    assert!(check(Some(fx.main[4])));
    assert!(!check(Some(fx.main[5])));
    assert!(check(Some(side[2])));
    // No locator: every block below the tip down to genesis.
    assert!(check(None));
    // Before genesis there is nothing to check.
    let empty = ScriptedFeeds::new(4, Vec::new());
    assert!(!prune_violation(&*empty.snapshot(), &fx.tree, None));
}

/// An index behind V's tip whose catch-up needs a pruned block refuses to open, with Core's text.
#[test]
fn open_refuses_a_pruned_catch_up() {
    let fx = fx(10);
    fx.tree.prune(fx.main[0]);
    let Err(err) = SubscriberDriver::open(
        ToyIndex::mandatory(),
        Arc::clone(&fx.db),
        Arc::clone(&fx.hub),
        Arc::new(NoBlocks),
        cfg(),
    ) else {
        panic!("opened");
    };
    assert!(err.to_string().ends_with(PRUNE_VIOLATION), "{err}");
    // It registered first, for the prune lock, and left on the refusal.
    assert!(fx.hub.actual_locks(&fx.main[10]).is_empty());
}

/// A txindex-shaped data object: inexact rows only (idempotent overwrites, DEC-77 Q6).
struct TxShaped;

static TX_ROWS: crate::subscriber::RowSchema = crate::subscriber::RowSchema {
    height_cfs: &[],
    archive_cfs: &[],
    state_keys: &[],
    keyed_cfs: &[],
    inexact_cfs: &[crate::subscriber::TOY_INEXACT],
    has_files: false,
    locator: (crate::subscriber::TOY_META, crate::subscriber::TOY_BEST),
};

impl SubscriberData for TxShaped {
    type E = ProbeEngine;
    type State = ();

    fn name(&self) -> &'static str {
        "txshaped"
    }

    fn needs(&self) -> dash_chain::Needs {
        dash_chain::Needs::default()
    }

    fn start_height(&self) -> u32 {
        0
    }

    fn schema(&self) -> &'static crate::subscriber::RowSchema {
        &TX_ROWS
    }

    fn initial_state(&self) {}

    fn connect(&self, b: &ConnectedBlock<'_>, (): &()) -> Result<(BlockRows, ()), SubscriberError> {
        let row = Row::Put(
            crate::subscriber::TOY_INEXACT,
            b.block.hash.as_bytes().to_vec(),
            b.block.height.to_be_bytes().to_vec(),
        );
        Ok((
            BlockRows {
                rows: vec![row],
            },
            (),
        ))
    }

    fn stage_disconnected(
        &self,
        _: &DisconnectedBlock<'_>,
        (): &(),
        _: &DurableBelow<'_, ProbeEngine>,
    ) -> Result<(Vec<Row>, ()), SubscriberError> {
        Ok((Vec::new(), ()))
    }

    fn archive(&self, _: &[Row]) -> Vec<Row> {
        Vec::new()
    }

    fn encode_state(&self, (): &()) -> Vec<Row> {
        Vec::new()
    }

    fn decode_state(
        &self,
        _: &[crate::subscriber::data::StateRead],
        _: &DurableBelow<'_, ProbeEngine>,
    ) -> Result<(), SubscriberError> {
        Ok(())
    }

    fn height_ranges(&self, _: u32, _: u32) -> Vec<crate::subscriber::data::CfRange> {
        Vec::new()
    }

    fn max_entry_bytes(&self) -> u64 {
        0
    }
}

/// C8 (k): a txindex-shaped subscriber keeps no journal across 10⁵ blocks, catching up and then
/// live, and its locator follows the commits.
#[test]
fn an_inexact_only_subscriber_keeps_no_journal() {
    const N: usize = 100_000;
    let fx = fx(N);
    fx.flush(1);
    let mut d = SubscriberDriver::open(
        TxShaped,
        Arc::clone(&fx.db),
        Arc::clone(&fx.hub),
        Arc::new(NoBlocks),
        tiny(),
    )
    .expect("opens");
    let mut steps = 0;
    while d.sync_step().expect("syncs") != SyncProgress::Live {
        assert_eq!((d.journal_len(), d.journal_bytes()), (0, 0));
        steps += 1;
        assert!(steps <= N + 1, "never live");
    }
    assert_eq!(d.class(), SubscriberClass::Synced);
    assert_eq!(d.durable(), Some(fx.main[N]));
    for b in fx.tree.branch(fx.main[N], 3) {
        fx.feeds.connect(b);
    }
    loop {
        let s = d.step().expect("steps");
        assert_eq!((d.journal_len(), d.journal_bytes()), (0, 0));
        if s != SubStep::Progress {
            assert_eq!(s, SubStep::Idle, "never parks without a journal");
            break;
        }
    }
    fx.flush(2);
    let mut s = d.step().expect("steps");
    while s == SubStep::Progress {
        s = d.step().expect("steps");
    }
    assert_eq!(s, SubStep::Idle);
    assert_eq!(d.durable(), fx.feeds.tip());
}

/// An optional subscriber Synced at 4, then three connects and a flush (2) that overtakes them:
/// drained, it has followed the backlog to the flushed tip (D38).
fn optional_flush_overtakes_backlog(cfg: DriverConfig) -> (Fx, Toy, BlockRef) {
    let fx = fx(4);
    fx.flush(1);
    let mut d = SubscriberDriver::open(
        ToyIndex::with(false, 0),
        Arc::clone(&fx.db),
        Arc::clone(&fx.hub),
        Arc::new(NoBlocks),
        cfg,
    )
    .expect("opens");
    sync(&mut d);
    assert_eq!(d.durable(), Some(fx.main[4]));
    let added = fx.tree.branch(fx.main[4], 3);
    for &b in &added {
        fx.feeds.connect(b);
    }
    assert!(fx.flush(2).is_empty(), "an optional subscriber is not awaited");
    assert_eq!(drain(&mut d), SubStep::Idle);
    let tip = added.last().copied().expect("added");
    assert_eq!(d.progress(), Some(tip));
    (fx, d, tip)
}

/// Sol r1's major 4: an optional subscriber's `ChainStateFlushed` comes after the data events
/// enqueued before it (Core's one validation queue), so the locator follows the backlog.
#[test]
fn an_optional_subscribers_flush_follows_its_backlog() {
    let (_fx, d, tip) = optional_flush_overtakes_backlog(cfg());
    assert_eq!(d.durable(), Some(tip), "the flush follows the queued connects");
}

/// Sol r1 major 3 (D37): step 5 reads the synced set after the handoff's final commit and before
/// its cut, so the subscriber gets no control; after the cut it rechecks `T_ack` and commits at
/// the flushed tip with no later tip event.
#[test]
#[expect(clippy::disallowed_methods, reason = "a plain channel out of the hook")]
fn a_handoff_step_5_misses_rechecks_t_ack() {
    let fx = fx(6);
    fx.hub.publish_durable(1, fx.main[4], 0);
    let mut d = fx.open(cfg());
    let hub = Arc::clone(&fx.hub);
    let (tip, seq) = (fx.main[6], fx.feeds.tip_seq());
    let (tx, rx) = std::sync::mpsc::channel();
    fx.feeds.set_after_finish(move || {
        tx.send(hub.publish_durable(2, tip, seq)).expect("sends");
    });
    sync(&mut d);
    assert_eq!(rx.recv().expect("step 5 ran"), Vec::new(), "the read missed the handoff");
    assert_eq!(d.durable(), Some(fx.main[4]), "the final commit used the old cap");
    assert_eq!(drain(&mut d), SubStep::Idle);
    assert_eq!(d.durable(), Some(tip));
    assert_eq!(fx.hub.durable_locator(d.id()), Some(tip));
}

/// Sol r1 major 4 at the limit (D38): an optional subscriber whose flush overtook its backlog
/// checkpoints at p toward the flushed tip instead of parking with `FlushDue`.
#[test]
fn an_optional_subscribers_flush_target_checkpoints_at_the_limit() {
    let (fx, d, tip) = optional_flush_overtakes_backlog(tiny());
    assert_eq!(d.durable(), Some(tip));
    assert!(!fx.hub.take_flush_due(), "no FlushDue: the checkpoints made room");
}

/// Sol r1 minor 5 (D39): `open` blocks on holds and flushes, so it is refused on a thread that
/// steps a registered subscriber, and allowed on another thread or once that one is gone.
#[test]
fn open_is_refused_on_a_thread_that_steps_a_subscriber() {
    let fx = fx(4);
    fx.flush(1);
    let mut d = fx.open(cfg());
    assert!(!fx.hub.steps_on_this_thread(), "not stepped yet");
    sync(&mut d);
    let other = || {
        SubscriberDriver::open(
            ToyIndex::with(false, 0),
            guarded(ProbeEngine::new(&TOY_DB)),
            Arc::clone(&fx.hub),
            Arc::new(NoBlocks),
            cfg(),
        )
    };
    let err = other().expect_err("refused here");
    assert!(err.to_string().ends_with(STEPPING_THREAD), "{err}");
    std::thread::scope(|s| {
        s.spawn(|| drop(other().expect("opens on another thread")));
    });
    drop(d);
    drop(other().expect("opens once the stepped one is gone"));
}

/// Sol r2's minor 3 (D47): a driver opened here and never stepped is still this thread's, so
/// another `open` here while a flush's window waits on it (a gate it has not rewound) is
/// refused rather than waiting on itself; on another thread it waits for the release.
#[test]
#[expect(clippy::disallowed_methods, reason = "plain test threads standing in for W and others")]
fn open_is_refused_on_the_owner_of_an_unstepped_gated_driver() {
    let fx = fx(10);
    fx.hub.publish_durable(1, fx.main[10], 0);
    let mut d = fx.open(cfg());
    sync(&mut d);
    assert_eq!(d.durable(), Some(fx.main[10]));
    drop(d);
    // Reopened here at L = 10 and never stepped: D39's stepper check has nothing to go on.
    let d = fx.open(cfg());
    assert!(!fx.hub.steps_on_this_thread());
    let fx = &fx;
    let other = || {
        SubscriberDriver::open(
            ToyIndex::with(false, 0),
            guarded(ProbeEngine::new(&TOY_DB)),
            Arc::clone(&fx.hub),
            Arc::new(NoBlocks),
            cfg(),
        )
    };
    std::thread::scope(|sc| {
        let w = sc.spawn(|| fx.hub.open_rewind_gate(3, fx.main[6]));
        assert_eq!(w.join().expect("W"), [d.id()]);
        // Without the refusal the open waits on itself: the watchdog's release turns that into
        // a failure, not a hang.
        let (done, watched) = std::sync::mpsc::channel::<()>();
        sc.spawn(move || {
            if watched.recv_timeout(Duration::from_secs(30)).is_err() {
                fx.hub.end_flush(3);
            }
        });
        let err = other().expect_err("refused on the owner's thread");
        assert!(err.to_string().contains("WouldDeadlock"), "{err}");
        done.send(()).expect("sent");
        let (tx, rx) = std::sync::mpsc::channel();
        sc.spawn(move || tx.send(other().map(drop).is_ok()).expect("sent"));
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err(), "elsewhere it waits");
        fx.hub.end_flush(3);
        assert!(rx.recv_timeout(Duration::from_secs(60)).expect("opened after the release"));
    });
    drop(d);
}

/// Sol r1 major 3's other order (D37): step 5's synced read misses the subscriber, which then
/// hands off before step 5 goes on. `T_ack` was published before the read, so the handoff's
/// final commit and its recheck see `new`.
#[test]
#[expect(clippy::disallowed_methods, reason = "plain test threads standing in for W")]
fn step_5_publishes_t_ack_before_its_synced_read() {
    let fx = fx(6);
    fx.hub.publish_durable(1, fx.main[4], 0);
    let mut d = fx.open(cfg());
    let (tip, seq) = (fx.main[6], fx.feeds.tip_seq());
    let (read_tx, read_rx) = std::sync::mpsc::channel();
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    fx.hub.set_after_synced_read(move || {
        read_tx.send(()).expect("sends");
        go_rx.recv().expect("released");
    });
    std::thread::scope(|sc| {
        let w = sc.spawn(|| fx.hub.publish_durable(2, tip, seq));
        read_rx.recv().expect("step 5 read the synced set");
        sync(&mut d);
        go_tx.send(()).expect("sends");
        assert_eq!(w.join().expect("W"), Vec::new(), "the read missed the handoff");
    });
    assert_eq!(drain(&mut d), SubStep::Idle);
    assert_eq!(d.durable(), Some(tip));
}

/// Sol r2 blocker 1 (D45): a mandatory subscriber stopped at L = 9 is not registered when a
/// backward flush makes G = 6; its next open rewinds L to the fork durably before it returns,
/// so a power loss right after the open finds L on G's path.
#[test]
fn open_after_a_backward_flush_rewinds_l_to_the_durable_tip() {
    let fx = fx(10);
    fx.hub.publish_durable(1, fx.main[9], 0);
    let mut d = fx.open(cfg());
    sync(&mut d);
    assert_eq!(d.durable(), Some(fx.main[9]));
    drop(d);
    for _ in 0..4 {
        fx.feeds.disconnect();
    }
    assert!(fx.hub.publish_durable(2, fx.main[6], fx.feeds.tip_seq()).is_empty());
    let d = fx.open(cfg());
    assert_eq!(d.durable(), Some(fx.main[6]), "L at the fork with G");
    assert_eq!(fx.hub.durable_locator(d.id()), Some(fx.main[6]));
    assert_eq!(d.best_block(), Some(fx.main[6]));
    fx.check_after_power_loss(d);
}

/// D45: an optional subscriber keeps Core's lazy rewind; its catch-up moves off the stale L.
#[test]
fn an_optional_open_after_a_backward_flush_keeps_its_locator() {
    let fx = fx(10);
    fx.hub.publish_durable(1, fx.main[9], 0);
    let mut d = fx.open_optional(cfg());
    sync(&mut d);
    drop(d);
    for _ in 0..4 {
        fx.feeds.disconnect();
    }
    fx.hub.publish_durable(2, fx.main[6], fx.feeds.tip_seq());
    let mut d = fx.open_optional(cfg());
    assert_eq!(d.durable(), Some(fx.main[9]), "Core's Init keeps the stale locator");
    sync(&mut d);
    assert_eq!(d.durable(), Some(fx.main[6]), "its sync rewinds it");
}

/// Sol r2 major 2 (D46): an optional subscriber's flush control overtakes a reorg backlog
/// (A6 A7 out, B6 B7 in); it applies every queued event, as Core's one queue does, so the stale
/// blocks it connected are archived at their disconnects, then commits at the flushed tip.
#[test]
fn an_optional_flush_over_a_reorg_backlog_keeps_the_archives() {
    let fx = fx(5);
    fx.flush(1);
    let mut d = fx.open_optional(cfg());
    sync(&mut d);
    assert_eq!(d.durable(), Some(fx.main[5]));
    let stale = fx.tree.branch(fx.main[5], 2);
    let side = fx.tree.branch(fx.main[5], 2);
    for &b in &stale {
        fx.feeds.connect(b);
    }
    fx.feeds.disconnect();
    fx.feeds.disconnect();
    for &b in &side {
        fx.feeds.connect(b);
    }
    assert!(fx.flush(2).is_empty());
    while drain(&mut d) == SubStep::Idle && !fx.feeds.pending_is_empty() {
        fx.feeds.retry_pending();
    }
    assert_eq!(d.progress(), Some(side[1]));
    assert_eq!(d.durable(), Some(side[1]), "the flushed tip, after the backlog");
    for &b in &stale {
        let (_, s) = toy_expected(&fx.path(b));
        let (k, v) = toy_archived(&b, &s);
        assert_eq!(fx.db.read(TOY_ARCHIVE, &k).expect("reads"), Some(v), "archived {}", b.height);
    }
    toy_check_at_l(&*fx.db, &fx.path(side[1])).expect("the state at L");
}

/// The validator's blocker on D46: an optional subscriber at its journal limit, with a stale leg
/// filling the queue (Q = 4) and V's fanout pending behind it (the disconnects included), must
/// not park for good. The flush V answers with checkpoints at p, on the target's leg, and
/// connects the first stale block; the next one, off the leg at the limit with its round trip
/// not yet queued, is dropped (D49), so the queue drains and the leg is disconnected.
#[test]
fn an_optional_stale_leg_at_the_limit_drains_vs_fanout() {
    let fx = fx(5);
    fx.hub.publish_durable(1, fx.main[4], 0);
    let mut d = fx.open_optional(tiny());
    sync(&mut d);
    assert_eq!((d.progress(), d.durable()), (Some(fx.main[5]), Some(fx.main[4])));
    let stale = fx.tree.branch(fx.main[5], 6);
    for &b in &stale {
        fx.feeds.connect(b);
    }
    for _ in &stale {
        fx.feeds.disconnect();
    }
    assert!(!fx.feeds.pending_is_empty(), "V's fanout waits on the queue");
    assert_eq!(drain(&mut d), SubStep::Parked, "the first stale block meets the limit");
    assert!(fx.hub.take_flush_due());
    assert!(fx.flush(2).is_empty());
    for _ in 0..100 {
        let s = drain(&mut d);
        if s == SubStep::Idle && fx.feeds.pending_is_empty() {
            break;
        }
        fx.feeds.retry_pending();
    }
    assert!(fx.feeds.pending_is_empty(), "V's fanout drained");
    assert_eq!(d.progress(), Some(fx.main[5]));
    assert_eq!(d.durable(), Some(fx.main[5]), "the checkpoint, then the flushed tip");
    let (_, s0) = toy_expected(&fx.path(stale[0]));
    let (k, v) = toy_archived(&stale[0], &s0);
    assert_eq!(fx.db.read(TOY_ARCHIVE, &k).expect("reads"), Some(v), "the applied stale block");
    toy_check_at_l(&*fx.db, &fx.path(fx.main[5])).expect("the state at L");
}

/// The validator's second blocker (D50): a flush that answers `FlushDue` and writes nothing
/// (here a target at L) must still count as the answer, or the next park posts nothing and V's
/// backpressure wait never ends. An optional subscriber at L = p = 5 with `d_max` 2 posts at
/// A2, V backs out to 5 and flushes there, then B3 meets the bound.
#[test]
fn a_flush_that_writes_nothing_still_answers_flush_due() {
    let fx = fx(5);
    fx.flush(1);
    let mut d = fx.open_optional(DriverConfig {
        d_max: Some(2),
        ..cfg()
    });
    sync(&mut d);
    assert_eq!(drain(&mut d), SubStep::Idle);
    assert_eq!(d.durable(), Some(fx.main[5]));
    let a = fx.tree.branch(fx.main[5], 2);
    for &b in &a {
        fx.feeds.connect(b);
    }
    assert_eq!(drain(&mut d), SubStep::Idle);
    assert!(fx.hub.take_flush_due(), "A2 meets the bound");
    fx.feeds.disconnect();
    fx.feeds.disconnect();
    assert!(fx.flush(2).is_empty());
    assert_eq!(drain(&mut d), SubStep::Idle);
    assert_eq!((d.progress(), d.durable()), (Some(fx.main[5]), Some(fx.main[5])), "no write");
    for &b in &fx.tree.branch(fx.main[5], 3) {
        fx.feeds.connect(b);
    }
    assert_eq!(drain(&mut d), SubStep::Parked);
    assert!(fx.hub.take_flush_due(), "the park posts again");
}

/// D46: a flushed target off p's branch is skipped, as Core's `ChainStateFlushed` skips a
/// locator not on the index's best chain; nothing is committed at the fork.
#[test]
fn a_flushed_target_off_ps_branch_is_skipped() {
    let fx = fx(5);
    // Sequence numbers start at the handoff: 0 is before it, the two connects are 1 and 2.
    fx.hub.publish_durable(1, fx.main[3], 0);
    let mut d = fx.open_optional(cfg());
    sync(&mut d);
    assert_eq!(drain(&mut d), SubStep::Idle);
    assert_eq!(d.durable(), Some(fx.main[3]));
    let a = fx.tree.branch(fx.main[5], 2);
    for &b in &a {
        fx.feeds.connect(b);
    }
    assert_eq!(drain(&mut d), SubStep::Idle);
    // A flushed tip on another branch, which the queue never reaches: L stays, where the LCA
    // commit would have moved it to 4.
    let off = fx.tree.branch(fx.main[4], 1)[0];
    fx.hub.publish_durable(2, off, 2);
    assert_eq!(drain(&mut d), SubStep::Idle);
    assert_eq!(d.durable(), Some(fx.main[3]), "Core warns and skips");
}
