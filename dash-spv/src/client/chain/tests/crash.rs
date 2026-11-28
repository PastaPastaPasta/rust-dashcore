//! Crash points of chain creation, the legacy reset, migration and replacement (PLAN-r0 §4.5):
//! a cut after every store event of the workload, as a process kill and as power losses, then
//! a reopen, the check of what booted, and the host's open again; a cut after the anchor's
//! first step; a kill, a reopen that acts, and a cut; and a header step cut at every store
//! event up to its publication.

use dash_kv::faulty::{AllSurvive, CrashKind, Decider, FaultyEngine, Var, World, WorldConfig};

use super::*;
use crate::storage::store::crash::{open_store, Config};
use crate::storage::store::hdr::io::FaultyIo;

type Faulty = Arc<Store<FaultyEngine, FaultyIo>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Work {
    /// A fresh store's open, masternode sync on (Candidate) or off (Established).
    Create {
        mn: bool,
    },
    /// A legacy store's open, masternode sync off: the legacy reset, recovery, the anchor's
    /// first step and the establish.
    Migrate,
    /// The legacy reset alone, so its uncut run is the cut between its batch and recovery.
    LegacyReset,
    /// Replacement's store job on a Candidate store with subscriber rows, 20000 to 10000.
    Replace,
}

struct LoseAll;

impl Decider for LoseAll {
    fn choose(&mut self, var: &Var) -> u32 {
        var.domain - 1
    }
}

/// Each pending write in turn kept, lost, and (where torn writes are possible) torn.
struct Alternate(u32);

impl Decider for Alternate {
    fn choose(&mut self, var: &Var) -> u32 {
        self.0 += 1;
        self.0 % var.domain
    }
}

fn cuts() -> Vec<(CrashKind, Box<dyn Decider>)> {
    vec![
        (CrashKind::ProcessKill, Box::new(LoseAll)),
        (CrashKind::PowerLoss, Box::new(LoseAll)),
        (CrashKind::PowerLoss, Box::new(AllSurvive)),
        (CrashKind::PowerLoss, Box::new(Alternate(0))),
    ]
}

/// Once armed with `n`, the world freezes after `n + 1` more store events: every later call
/// fails without changing anything.
#[derive(Clone, Default)]
struct Killer(Arc<Mutex<Option<u64>>>);

impl Killer {
    fn world(&self) -> World {
        let left = Arc::clone(&self.0);
        World::new(WorldConfig {
            on_event: Some(Box::new(move |w: &World, _| {
                let mut left = left.lock();
                match left.as_mut() {
                    Some(0) => {
                        w.freeze();
                        *left = None;
                    }
                    Some(n) => *n -= 1,
                    None => {}
                }
            })),
            ..WorldConfig::default()
        })
    }

    fn arm(&self, n: u64) {
        *self.0.lock() = Some(n);
    }

    /// The store events left before the cut, or `None` once it fired.
    fn left(&self) -> Option<u64> {
        *self.0.lock()
    }

    /// Disarms; whether the cut was reached.
    fn fired(&self) -> bool {
        self.0.lock().take().is_none()
    }
}

fn store(world: &World) -> Faulty {
    Arc::new(open_store(world, &Config::default()).unwrap_or_else(|_| panic!("the store opens")))
}

/// The store closed, the world cut by `kind`, and the store reopened on what booted.
fn crash(world: &World, h: Faulty, kind: CrashKind, decider: Box<dyn Decider>) -> Faulty {
    drop(h);
    world.crash_in_place(kind, decider);
    store(world)
}

fn old() -> AnchorPack {
    pack(20000, 1_000_000)
}

fn new() -> AnchorPack {
    pack(10000, 1_000_000)
}

fn legacy_pack() -> AnchorPack {
    pack(15000, 1_000_000)
}

fn host(work: Work) -> ChainHostSetup {
    match work {
        Work::Create {
            mn,
        } => setup(mn, None),
        Work::Migrate | Work::LegacyReset => setup(false, None),
        Work::Replace => setup(true, None),
    }
}

/// The store before the workload, made durable.
fn prepare(world: &World, work: Work) {
    let h = store(world);
    match work {
        Work::Create {
            ..
        } => {}
        Work::Migrate | Work::LegacyReset => fill_legacy(&h, &legacy_pack()),
        Work::Replace => {
            drop(open_chain(&h, &host(work)).unwrap());
            put_subs(&h, old().height);
        }
    }
    h.sync_wal().unwrap();
}

fn act(h: &Faulty, work: Work) -> Result<(), SpvError> {
    let s = host(work);
    match work {
        Work::Replace => replace_chain(h, &s, &pack_anchor(&new()), &GenGuard::default()).map(drop),
        Work::LegacyReset => h
            .reset_legacy(candidate(&s, &pack_anchor(&new())))
            .map_err(|e| SpvError::Storage(e.into())),
        _ => open_chain(h, &s).map(drop),
    }
}

/// The anchor the workload resets to, and its state once the host's open has run.
fn target(work: Work) -> (AnchorPack, AnchorState) {
    match work {
        Work::Create {
            mn,
        } => (
            old(),
            if mn {
                AnchorState::Candidate
            } else {
                AnchorState::Established
            },
        ),
        Work::Migrate | Work::LegacyReset => (new(), AnchorState::Established),
        Work::Replace => (new(), AnchorState::Candidate),
    }
}

/// The store a completed workload leaves: the anchor's state, and whether its chain is built.
fn uncut(work: Work) -> (AnchorState, bool) {
    match work {
        Work::LegacyReset => (AnchorState::Candidate, false),
        _ => (target(work).1, true),
    }
}

fn hashes(pack: &AnchorPack) -> Vec<BlockHash> {
    pack.headers.iter().map(|h| Header::new(*h).hash()).collect()
}

fn assert_legacy_intact(h: &Faulty) {
    assert_eq!(h.legacy_start(), Some(legacy_pack().height - 24));
    for hash in hashes(&legacy_pack()) {
        assert!(h.by_hash(&hash).unwrap().is_some(), "a legacy header was lost");
    }
    assert!(!h.view().sub(Subscriber::FilterHeaders).is_empty());
    assert!(h.locator(Subscriber::FilterHeaders).unwrap().is_some());
    assert!(!h.view().sub(Subscriber::Filters).is_empty());
}

/// What a store may hold after a cut, before any repair:
/// - with no `chain` marker, the store as the workload found it: fresh, or legacy with its
///   headers and subscriber rows;
/// - with the marker, no legacy header and a `meta` row: replacement's old chain whole with
///   its subscriber rows, or the workload's anchor with no subscriber row, either reset with
///   no record yet (Candidate) or the anchor's chain whole.
///
/// The anchor it holds.
fn check_booted(h: &Faulty, work: Work) -> AnchorPack {
    let (to, _) = target(work);
    if !h.marker_stored() {
        match work {
            Work::Create {
                ..
            } => {
                assert_eq!((h.view().active.hi, h.min_height().unwrap()), (None, None));
                assert_eq!(h.chain_meta().unwrap(), None);
                return to;
            }
            Work::Migrate | Work::LegacyReset => {
                assert_legacy_intact(h);
                return legacy_pack();
            }
            Work::Replace => panic!("a replaced store lost its marker"),
        }
    }
    assert_eq!(h.legacy_start(), None);
    for hash in hashes(&legacy_pack()) {
        assert!(h.by_hash(&hash).unwrap().is_none(), "a legacy header beside the marker");
    }
    let m = h.chain_meta().unwrap().expect("a meta row beside the marker");
    if work == Work::Replace && m.anchor == (old().hash, old().height) {
        assert_anchored(&**h, &old(), AnchorState::Candidate);
        assert!(!h.view().sub(Subscriber::FilterHeaders).is_empty());
        return old();
    }
    assert_eq!(m.anchor, (to.hash, to.height), "{work:?}");
    assert_no_subs(&**h);
    match h.view().active.hi {
        None => {
            assert_eq!(m.anchor_state, AnchorState::Candidate, "Established with no anchor");
            assert_eq!(h.min_height().unwrap(), None, "a context record without its anchor");
        }
        Some(_) => {
            assert_ne!(work, Work::LegacyReset);
            assert_anchored(&**h, &to, m.anchor_state);
            assert!(StoredChain::read(&**h).is_ok());
        }
    }
    to
}

/// The host's open after the cut: the chain at the anchor the store booted with, or the
/// workload's when it booted with nothing of it yet.
fn recover(h: &Faulty, work: Work, booted: &AnchorPack) {
    let s = host(work);
    let (chain, _) = open_chain(h, &s).unwrap_or_else(|e| panic!("{work:?}: reopen: {e}"));
    let (to, state) = target(work);
    let kept = work == Work::Replace && booted.height == old().height;
    let at = if kept {
        old()
    } else {
        to
    };
    assert_eq!(tip(&chain), at.hash, "{work:?} booted at {}", booted.height);
    assert_anchored(&**h, &at, state);
    if !kept {
        assert_no_subs(&**h);
    }
    assert!(!s.fail.is_tripped());
}

/// One cut after event `n + 1` of the workload; whether the cut was reached. Every commit of
/// a workload is Durable, so an uncut one survives any cut whole.
fn schedule(work: Work, n: u64, kind: CrashKind, decider: Box<dyn Decider>) -> bool {
    let kill = Killer::default();
    let world = kill.world();
    prepare(&world, work);
    let h = store(&world);
    kill.arm(n);
    let done = act(&h, work);
    let fired = kill.fired();
    if !fired {
        done.unwrap_or_else(|e| panic!("{work:?} uncut: {e}"));
    }
    let h = crash(&world, h, kind, decider);
    let booted = check_booted(&h, work);
    if !fired {
        let (to, _) = target(work);
        let (state, built) = uncut(work);
        assert_eq!(booted.hash, to.hash, "{work:?}: the uncut workload's anchor");
        assert!(h.marker_stored(), "{work:?}: the uncut workload's marker");
        assert_eq!(meta(&*h).anchor_state, state, "{work:?} uncut");
        assert_eq!(h.view().active.hi.is_some(), built, "{work:?} uncut, {kind:?}");
    }
    recover(&h, work, &booted);
    fired
}

/// Every cut point of `work`, under every cut; the number of cut points.
fn every_cut(work: Work) -> u64 {
    let mut n = 0;
    loop {
        let mut fired = false;
        for (kind, decider) in cuts() {
            fired |= schedule(work, n, kind, decider);
        }
        if !fired {
            eprintln!("{work:?}: {n} cut points");
            return n;
        }
        n += 1;
    }
}

#[tokio::test]
async fn a_failed_write_in_a_replacement_trips_the_fail_stop_and_stops_the_task() {
    let world = World::new(WorldConfig::default());
    prepare(&world, Work::Replace);
    let h = store(&world);
    let s = host(Work::Replace);
    let host = SpvChainHost::open_on(h.clone(), s.clone()).await.unwrap();
    let mut source = host.source();
    world.freeze();
    let out = host.handle().replace(pack_anchor(&new())).await;
    assert!(matches!(out, ReplaceOutcome::FailedDurably(_)), "{out:?}");
    assert!(s.fail.is_tripped());
    source.wait_for(|now| matches!(now.state, SourceState::Stopped)).await.unwrap();
    assert_eq!(source.borrow().gen, ChainGen(1));
    assert!(matches!(
        host.handle().replace(pack_anchor(&new())).await,
        ReplaceOutcome::FailedDurably(SpvError::ChannelFailure(..))
    ));
}

/// PLAN-r1 §2.2.2: a failed commit publishes nothing, trips the fail-stop and stops the task.
#[tokio::test]
async fn a_failed_step_commit_publishes_nothing_and_stops_the_task() {
    let world = World::new(WorldConfig::default());
    prepare(
        &world,
        Work::Create {
            mn: false,
        },
    );
    let h = store(&world);
    let s = host(Work::Create {
        mn: false,
    });
    let host = SpvChainHost::open_on(h.clone(), s.clone()).await.unwrap();
    let mut source = host.source();
    let SourceState::Live {
        reader,
        ..
    } = source.borrow().state.clone()
    else {
        panic!("not live");
    };
    let before = reader.snapshot();
    let frozen = world.clone();
    h.interleave(Point::WriterWaiting, move |_| frozen.freeze());
    let out = host
        .handle()
        .headers(step::PEER, step::bridged(&step::mine(old().hash, 1_000_000, 2, 1)))
        .await;
    assert!(matches!(out, Err(SpvError::Storage(_))), "{out:?}");
    assert!(s.fail.reason().unwrap().starts_with("a header step"), "{:?}", s.fail.reason());
    source.wait_for(|now| matches!(now.state, SourceState::Stopped)).await.unwrap();
    let after = reader.snapshot();
    assert_eq!((after.epoch, after.tip.tip_hash), (before.epoch, old().hash));
}

#[test]
fn a_fresh_store_recovers_at_every_crash_point_of_its_creation() {
    for mn in [true, false] {
        let points = every_cut(Work::Create {
            mn,
        });
        assert!(points > 3, "{points}");
    }
}

#[test]
fn a_legacy_reset_recovers_at_every_event_of_its_batch_and_before_recovery() {
    let points = every_cut(Work::LegacyReset);
    assert!(points > 1, "{points}");
}

#[test]
fn a_legacy_store_recovers_at_every_crash_point_of_its_migration() {
    let points = every_cut(Work::Migrate);
    assert!(points > every_cut(Work::LegacyReset), "{points}");
}

#[test]
fn a_replacement_recovers_at_every_crash_point_of_its_reset_and_build() {
    let points = every_cut(Work::Replace);
    assert!(points > 3, "{points}");
}

/// A cut once the anchor's first step returned, before the establish: the step is Durable,
/// so every cut keeps the anchor's chain, still a Candidate.
#[test]
fn a_cut_after_the_anchors_first_step_keeps_it() {
    for work in [
        Work::Create {
            mn: false,
        },
        Work::Migrate,
    ] {
        for (kind, decider) in cuts() {
            let world = World::new(WorldConfig::default());
            prepare(&world, work);
            let h = store(&world);
            let frozen = world.clone();
            h.interleave(Point::EstablishUnlocked, move |_| frozen.freeze());
            assert!(act(&h, work).is_err(), "{work:?}: the establish was cut");
            let h = crash(&world, h, kind, decider);
            let (to, _) = target(work);
            assert_eq!(check_booted(&h, work).hash, to.hash, "{work:?}, {kind:?}");
            assert_anchored(&*h, &to, AnchorState::Candidate);
            recover(&h, work, &to);
        }
    }
}

/// r3 N3's schedule: a kill at `n`, a reopen whose own open is killed at `m`, then a power
/// loss; the next open recovers.
#[test]
fn a_killed_migration_reopened_and_cut_again_recovers() {
    let work = Work::Migrate;
    let points = every_cut(work);
    for n in 0..points {
        for m in 0.. {
            let kill = Killer::default();
            let world = kill.world();
            prepare(&world, work);
            let h = store(&world);
            kill.arm(n);
            let _ = act(&h, work);
            assert!(kill.fired(), "a cut point below {points}");
            let h = crash(&world, h, CrashKind::ProcessKill, Box::new(LoseAll));
            check_booted(&h, work);
            kill.arm(m);
            let again = open_chain(&h, &host(work)).map(drop);
            let fired = kill.fired();
            let h = crash(&world, h, CrashKind::PowerLoss, Box::new(Alternate(m as u32)));
            let booted = check_booted(&h, work);
            recover(&h, work, &booted);
            if !fired {
                assert!(again.is_ok(), "{again:?}");
                break;
            }
        }
    }
}

/// The header steps cut, each on an Established anchor at 20000.
#[derive(Clone, Copy, Debug)]
enum Cut {
    /// Ten headers on the anchor.
    Extend,
    /// A1…A6 active, then B3…B8 on A2: the active rows above A2 change branch.
    Reorg,
    /// A1…A6 active and B3, B4 on A2 stored, then a ChainLock on B4: the tip moves to B4, A3 is
    /// marked and the ChainLock rows change.
    Lock,
}

/// What a cut boots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Boot {
    /// The whole old branch.
    Old,
    /// The whole new branch.
    New,
    /// A prefix of the old branch below its tip (BL-989).
    OldPrefix,
}

/// Classifies `t` booted active rows against the old branch of `before` rows and the new one of
/// `after`, given whether the rows are a prefix of each. A reorg's lowering commits each batch on
/// its own (SB14 D7), so a cut after one, or a power loss before the step's Buffered commit is
/// synced, boots at a prefix of the old branch, and a power loss that drops the earlier Buffered
/// steps boots lower. Any other boot is an error.
fn boot(
    t: usize,
    on_before: bool,
    on_after: bool,
    before: usize,
    after: usize,
) -> Result<Boot, String> {
    match t {
        t if on_after && t == after => Ok(Boot::New),
        t if on_before && t == before => Ok(Boot::Old),
        t if on_before && t < before => Ok(Boot::OldPrefix),
        t => Err(format!(
            "{t} rows, an old-branch prefix {on_before}, a new-branch prefix {on_after}"
        )),
    }
}

#[test]
fn a_boot_is_a_whole_branch_or_an_old_branch_prefix() {
    assert_eq!(boot(6, true, false, 6, 8), Ok(Boot::Old));
    assert_eq!(boot(8, false, true, 6, 8), Ok(Boot::New));
    assert_eq!(boot(0, true, true, 0, 10), Ok(Boot::Old));
    assert_eq!(boot(3, true, false, 6, 8), Ok(Boot::OldPrefix));
    assert_eq!(boot(2, true, true, 6, 8), Ok(Boot::OldPrefix));
    assert!(boot(5, false, true, 6, 8).is_err(), "a new-branch prefix beyond the fork");
    assert!(boot(5, false, true, 0, 10).is_err(), "a part of an extension");
    assert!(boot(5, false, false, 6, 8).is_err(), "rows of neither branch");
}

/// One cut of a header step: after store event `n + 1` of its commit, or, when the commit
/// returned first, of its publication, which makes no store event. What boots is one branch's
/// active rows with `meta.tip` at their top, never a mix ([`boot`]). Every boot reopens at that
/// top with the stored ChainLock state, the records and, for a lock, the ChainLock rows and
/// marks of the boot checked before and after a tick, which moves it to the fork target: the new
/// tip, or the top of the old branch's stored records. The chain then takes the next step. Whether the cut was reached, and what booted.
fn step_cut(cut: Cut, n: u64, kind: CrashKind, decider: Box<dyn Decider>) -> (bool, Boot) {
    let kill = Killer::default();
    let world = kill.world();
    let s = host(Work::Create {
        mn: false,
    });
    drop(open_chain(&store(&world), &s).unwrap());
    let h = store(&world);
    let (mut chain, _) = open_chain(&h, &s).unwrap();
    let parker = dash_chain::Parker::new();
    let now = dash_clock::NodeSeconds(2_000_000);
    let a = step::mine(old().hash, 1_000_000, 6, 1);
    let b = |n| step::mine(a[1].hash(), 1_000_000 + 2 * step::SPACING, n, 2);
    let (pre, input, before, after, added) = match cut {
        Cut::Extend => {
            let headers = step::mine(old().hash, 1_000_000, 10, 1);
            (vec![], step::batch(&headers), vec![], headers.clone(), headers)
        }
        Cut::Reorg => {
            let b = b(6);
            (vec![a.clone()], step::batch(&b), a.clone(), [&a[..2], &b].concat(), b)
        }
        Cut::Lock => {
            let b = b(2);
            let lock = dash_chain::ChainLockRef {
                height: 20004,
                hash: b[1].hash(),
            };
            let after = [&a[..2], &b].concat();
            (
                vec![a.clone(), b],
                super::super::step::StepInput::ChainLock(lock),
                a.clone(),
                after,
                vec![],
            )
        }
    };
    for headers in &pre {
        super::super::step::step_parked(&mut chain, &h, step::batch(headers), &parker, now)
            .unwrap();
    }
    kill.arm(n);
    let committed = super::super::step::commit(&mut chain, &h, input);
    let left = kill.left();
    if left.is_some() {
        let committed = committed.unwrap_or_else(|e| panic!("uncut: {e:?}"));
        let published = super::super::step::publish(
            &mut chain,
            committed,
            &mut super::super::step::StepWait::bare(&parker),
            now,
        );
        assert_eq!(kill.left(), left, "the publication makes no store event");
        published.unwrap();
    }
    let fired = kill.fired();
    drop(chain);
    let h = crash(&world, h, kind, decider);
    let hi = h.view().active.hi.expect("an active chain") - old().height;
    let on = |path: &[Header]| {
        hi as usize <= path.len()
            && (1..=hi).all(|i| {
                h.active_hash(old().height + i).unwrap()
                    == Some(*path[i as usize - 1].hash().as_bytes())
            })
    };
    let booted = boot(hi as usize, on(&before), on(&after), before.len(), after.len())
        .unwrap_or_else(|e| panic!("{cut:?} {n} {kind:?}: {e}"));
    let meta_tip = h.by_id(meta(&*h).tip).unwrap().map(|r| *r.hash.as_bytes());
    let active_top = h.active_hash(old().height + hi).unwrap();
    assert_eq!(meta_tip, active_top, "{cut:?} {n} {kind:?}: meta.tip off the active top");
    let moved = booted == Boot::New;
    let stored = before.iter().take_while(|x| h.by_hash(&x.hash()).unwrap().is_some()).count();
    assert!(stored == before.len() || stored == hi as usize, "{cut:?} {n} {kind:?}: {stored}");
    let path = if moved {
        &after
    } else {
        &before[..stored]
    };
    let top = |path: &[Header]| path.last().map_or(old().hash, Header::hash);
    let (mut chain, _) = open_chain(&h, &s).unwrap_or_else(|e| panic!("{n} {kind:?}: {e}"));
    assert_eq!(Some(*tip(&chain).as_bytes()), active_top, "{cut:?} {n} {kind:?}");
    let target = chain.fork_target(&chain.virtual_tip()).unwrap();
    assert_eq!(chain.reader().try_entry(target.id).unwrap().unwrap().hash, top(path));
    assert_eq!(chain.cl_state(), meta(&*h).cl, "{cut:?} {n} {kind:?}: the restore");
    let restored = |chain: &Chain<Faulty>, when: &str| {
        for header in &added {
            let held = h.by_hash(&header.hash()).unwrap().is_some();
            assert_eq!(held, moved, "{cut:?} {n} {kind:?} {when}");
        }
        if let Cut::Lock = cut {
            let r = chain.reader();
            let cl = chain.cl_state();
            assert_eq!(cl.best.is_some(), moved, "{n} {kind:?} {when}");
            let floor = cl.floor.map(|f| r.try_entry(f).unwrap().expect("held").hash);
            assert_eq!(floor, moved.then(|| top(&after)), "{n} {kind:?} {when}: the floor");
            match r.try_lookup(&a[2].hash()).unwrap() {
                Some(id) => {
                    let status = r.try_entry(id).unwrap().expect("A3 is held").status;
                    let marked = status.has(dash_chain::Status::CONFLICT_CHAINLOCK);
                    assert_eq!(marked, moved, "{n} {kind:?} {when}");
                }
                None => assert!(path.is_empty(), "{n} {kind:?} {when}: A3 is held"),
            }
        }
    };
    restored(&chain, "before the tick");
    super::super::step::step_parked(
        &mut chain,
        &h,
        super::super::step::StepInput::Tick,
        &parker,
        now,
    )
    .unwrap();
    assert_eq!(tip(&chain), top(path), "{cut:?} {n} {kind:?}: the tick");
    restored(&chain, "after the tick");
    assert_eq!(meta(&*h).anchor_state, AnchorState::Established);
    if !fired && kind == CrashKind::ProcessKill {
        assert!(moved, "{cut:?}: a committed step survives a process kill");
    }
    let next = step::mine(tip(&chain), 1_000_000 + step::SPACING * path.len() as u32, 1, 3);
    super::super::step::step_parked(&mut chain, &h, step::batch(&next), &parker, now)
        .unwrap_or_else(|e| panic!("{cut:?} {n} {kind:?}: the next step: {e}"));
    assert_eq!(tip(&chain), next[0].hash());
    assert!(!s.fail.is_tripped());
    (fired, booted)
}

/// Every cut point of `cut`'s step, until the step runs uncut: how many there were, and per
/// entry of `cuts()` how many boots, all reopened, were the old branch, the new one and an
/// old-branch prefix.
fn cut_every_point(cut: Cut) -> (u64, Vec<[u32; 3]>) {
    let mut n = 0;
    let mut counts = vec![[0; 3]; cuts().len()];
    loop {
        let mut fired = false;
        for (i, (kind, decider)) in cuts().into_iter().enumerate() {
            let (f, booted) = step_cut(cut, n, kind, decider);
            fired |= f;
            counts[i][booted as usize] += 1;
        }
        if !fired {
            break;
        }
        n += 1;
    }
    eprintln!("{cut:?} step: {n} cut points, [old, new, old prefix] {counts:?}");
    (n, counts)
}

#[test]
fn a_header_step_recovers_at_every_crash_point_up_to_its_publication() {
    assert_eq!(
        cut_every_point(Cut::Extend),
        (10, vec![[9, 2, 0], [11, 0, 0], [9, 2, 0], [11, 0, 0]])
    );
}

#[test]
fn a_reorg_step_recovers_at_every_crash_point_up_to_its_publication() {
    assert_eq!(
        cut_every_point(Cut::Reorg),
        (12, vec![[0, 2, 11], [0, 0, 13], [0, 2, 11], [1, 0, 12]])
    );
}

#[test]
fn a_chainlock_step_recovers_at_every_crash_point_up_to_its_publication() {
    assert_eq!(cut_every_point(Cut::Lock), (8, vec![[0, 2, 7], [0, 0, 9], [0, 2, 7], [1, 0, 8]]));
}

/// The first step after a prefix reopen.
#[derive(Clone, Copy, Debug)]
enum First {
    Tick,
    Headers,
    ChainLock,
}

/// A prefix boot's recovery (r2 m-b, BL-989): A1…A6, then the `cut` step's new branch on A2,
/// B3…B8 for a reorg and B3, B4 with a ChainLock on B4 for a lock, the B headers of a lock
/// stored first. The step is cut by a process kill before the lowering's second batch
/// (`LOWER_BATCH` = 3) when `mid_lowering`, else it runs whole and a power loss loses its
/// Buffered commit. The store boots at A3 or A2, with no ChainLock and no record of the step's;
/// the chain reopens there with A6 as its fork target. The `first` step, then the step's own
/// input delivered again if `first` was a tick, lands on the new branch, which a reopen keeps as
/// tip and fork target.
fn prefix_recovers(cut: Cut, mid_lowering: bool, first: First) {
    let world = World::new(WorldConfig::default());
    let s = host(Work::Create {
        mn: false,
    });
    drop(open_chain(&store(&world), &s).unwrap());
    let h = store(&world);
    let (mut chain, _) = open_chain(&h, &s).unwrap();
    let parker = dash_chain::Parker::new();
    let now = dash_clock::NodeSeconds(2_000_000);
    let run = super::super::step::step_parked;
    let a = step::mine(old().hash, 1_000_000, 6, 1);
    let b = |n| step::mine(a[1].hash(), 1_000_000 + 2 * step::SPACING, n, 2);
    run(&mut chain, &h, step::batch(&a), &parker, now).unwrap();
    let b = match cut {
        Cut::Reorg => b(6),
        Cut::Lock => {
            let b = b(2);
            run(&mut chain, &h, step::batch(&b), &parker, now).unwrap();
            b
        }
        Cut::Extend => unreachable!("an extension lowers nothing"),
    };
    let lock = dash_chain::ChainLockRef {
        height: 20004,
        hash: b[1].hash(),
    };
    let input = |kind: First| match kind {
        First::ChainLock => super::super::step::StepInput::ChainLock(lock),
        First::Headers => step::batch(&b),
        First::Tick => super::super::step::StepInput::Tick,
    };
    let own = match cut {
        Cut::Lock => First::ChainLock,
        _ => First::Headers,
    };
    let (kind, k) = match mid_lowering {
        true => {
            let w = world.clone();
            h.interleave(Point::LowerBatch(1), move |_| w.freeze());
            assert!(run(&mut chain, &h, input(own), &parker, now).is_err());
            (CrashKind::ProcessKill, 3)
        }
        false => {
            run(&mut chain, &h, input(own), &parker, now).unwrap();
            assert_eq!(tip(&chain), b.last().unwrap().hash());
            (CrashKind::PowerLoss, 2)
        }
    };
    drop(chain);
    let h = crash(&world, h, kind, Box::new(LoseAll));
    let at = format!("{cut:?} {kind:?} {first:?}");
    assert_eq!(h.view().active.hi, Some(old().height + k), "{at}");
    let b_held = matches!(cut, Cut::Lock);
    assert!(b.iter().all(|x| h.by_hash(&x.hash()).unwrap().is_some() == b_held), "{at}");
    assert_eq!(meta(&*h).cl.best, None, "{at}");

    let (mut chain, _) = open_chain(&h, &s).expect("an old-branch prefix reopens");
    assert_eq!(tip(&chain), a[k as usize - 1].hash(), "{at}");
    let target = |chain: &Chain<Faulty>| {
        let id = chain.fork_target(&chain.virtual_tip()).unwrap().id;
        chain.reader().try_entry(id).unwrap().unwrap().hash
    };
    assert_eq!(target(&chain), a[5].hash(), "{at}");
    run(&mut chain, &h, input(first), &parker, now).unwrap();
    if let First::Tick = first {
        assert_eq!(tip(&chain), a[5].hash(), "{at}: the tick");
        run(&mut chain, &h, input(own), &parker, now).unwrap();
    }
    let landed = |chain: &Chain<Faulty>, when: &str| {
        assert_eq!(tip(chain), b.last().unwrap().hash(), "{at} {when}");
        assert_eq!(target(chain), tip(chain), "{at} {when}: the fork target");
        if let Cut::Lock = cut {
            let r = chain.reader();
            assert_eq!(chain.cl_state().best, Some(lock), "{at} {when}");
            let a3 = r.try_lookup(&a[2].hash()).unwrap().expect("A3 is held");
            let status = r.try_entry(a3).unwrap().expect("A3 is held").status;
            assert!(status.has(dash_chain::Status::CONFLICT_CHAINLOCK), "{at} {when}");
        }
        assert!(!s.fail.is_tripped(), "{at} {when}");
    };
    landed(&chain, "recovered");
    drop(chain);
    let (chain, _) = open_chain(&h, &s).expect("the recovered store reopens");
    landed(&chain, "reopened");
}

#[test]
fn a_cut_mid_lowering_reopens_at_the_prefix_and_steps_to_the_fork_target() {
    prefix_recovers(Cut::Reorg, true, First::Tick);
}

#[test]
fn a_power_loss_after_a_reorg_step_reopens_at_the_fork_point_and_steps_on() {
    prefix_recovers(Cut::Reorg, false, First::Tick);
}

#[test]
fn a_prefix_boot_recovers_when_headers_come_first() {
    prefix_recovers(Cut::Reorg, true, First::Headers);
    prefix_recovers(Cut::Reorg, false, First::Headers);
}

#[test]
fn a_prefix_boot_recovers_when_a_chainlock_comes_first() {
    prefix_recovers(Cut::Lock, true, First::ChainLock);
}

#[test]
fn a_power_loss_that_lost_a_chainlock_step_recovers_when_the_lock_comes_again() {
    prefix_recovers(Cut::Lock, false, First::ChainLock);
    prefix_recovers(Cut::Lock, false, First::Tick);
}

/// The BL-989 seam: a store that booted at the A3 prefix, with A6 the tip it had published,
/// and a host on it with a subscriber whose first notice, `Resync { Start }`, is at A3, below
/// that tip; `a` and `b` as mined, `b` forking at A2. A3 is still the tip only while the host's
/// first tick, 5 s after its open, has not run: a wall-clock assumption of both tests.
async fn prefix_booted() -> (SpvChainHost, ChainSub, ChainHostSetup, Vec<Header>, Vec<Header>) {
    let world = World::new(WorldConfig::default());
    let s = host(Work::Create {
        mn: false,
    });
    drop(open_chain(&store(&world), &s).unwrap());
    let h = store(&world);
    let (mut chain, _) = open_chain(&h, &s).unwrap();
    let (parker, now) = (dash_chain::Parker::new(), dash_clock::NodeSeconds(2_000_000));
    let run = super::super::step::step_parked;
    let a = step::mine(old().hash, 1_000_000, 6, 1);
    let b = step::mine(a[1].hash(), 1_000_000 + 2 * step::SPACING, 6, 2);
    run(&mut chain, &h, step::batch(&a), &parker, now).unwrap();
    let w = world.clone();
    h.interleave(Point::LowerBatch(1), move |_| w.freeze());
    assert!(run(&mut chain, &h, step::batch(&b), &parker, now).is_err());
    drop(chain);
    let h = crash(&world, h, CrashKind::ProcessKill, Box::new(LoseAll));
    let host = SpvChainHost::open_on(h, s.clone()).await.unwrap();
    host.start_subscribers().await.unwrap();
    let mut sub = host.subscribe(SubKind::Blocks);
    let ChainNotice::Resync {
        pin,
        why: ResyncWhy::Start,
        ..
    } = super::sub::next(&mut sub).await
    else {
        panic!("not a start");
    };
    assert_eq!(pin.snap.tip.tip_hash, a[2].hash(), "the prefix, below A6");
    (host, sub, s, a, b)
}

/// The prefix boot with `b` sent at once: the notices follow, fenced, from A3 to the new
/// branch. A4 to A6 come back only if the host's tick steps to A6 before `b`'s step, and go
/// again at the reorg to B8.
#[tokio::test]
async fn a_subscriber_after_a_prefix_boot_starts_at_the_prefix() {
    let (host, mut sub, s, a, b) = prefix_booted().await;
    host.handle().headers(step::PEER, step::bridged(&b)).await.unwrap();
    let mut at = a[2].hash();
    while at != b[5].hash() {
        let n = super::sub::next(&mut sub).await;
        super::sub::fenced(&n);
        at = match n {
            ChainNotice::Advanced {
                from,
                to,
                ..
            } => {
                assert_eq!((from.hash, to.hash), (at, a[5].hash()), "the tick to A6");
                to.hash
            }
            ChainNotice::Reorg {
                fork,
                old_tip,
                new_tip,
                ..
            } => {
                assert_eq!((fork.hash, old_tip.hash), (a[1].hash(), at));
                new_tip.hash
            }
            n => panic!("{n:?}"),
        };
    }
    assert!(!s.fail.is_tripped());
}

/// The prefix boot, tick first: the host's first tick (5 s after its open) steps to the fork
/// target A6, which reconnects A4 to A6, and `b`'s step then disconnects them again at the
/// reorg to B8.
#[tokio::test]
async fn a_subscriber_after_a_prefix_boot_sees_the_tick_reconnect_then_the_reorg() {
    let (host, mut sub, s, a, b) = prefix_booted().await;
    let n = super::sub::next(&mut sub).await;
    super::sub::fenced(&n);
    let ChainNotice::Advanced {
        from,
        to,
        ..
    } = &n
    else {
        panic!("{n:?}");
    };
    assert_eq!((from.hash, to.hash), (a[2].hash(), a[5].hash()), "the tick to A6");
    host.handle().headers(step::PEER, step::bridged(&b)).await.unwrap();
    let n = super::sub::next(&mut sub).await;
    super::sub::fenced(&n);
    let ChainNotice::Reorg {
        fork,
        old_tip,
        new_tip,
        disconnected,
        connected,
        ..
    } = &n
    else {
        panic!("{n:?}");
    };
    assert_eq!(
        (fork.hash, old_tip.hash, new_tip.hash, *disconnected, *connected),
        (a[1].hash(), a[5].hash(), b[5].hash(), 4, 6)
    );
    assert!(!s.fail.is_tripped());
}
