//! LK1b-s S1e: the IS store's power-loss matrix (PLAN-LK1b §2.4). NodeSut runs the main
//! fixture over a FaultyEngine-backed IS store with `Recovery::Subset`, the run compared with
//! Core's as the differential compares it. At every commit of every op of scenarios (a)–(f), the
//! test takes the engine's cut (its durable base and its pending batches) and an image, and cuts
//! power on the image under several deciders. It reopens the store with empty caches and
//! `best_confirmed_height = 0`, and checks that the recovered rows are a state the oracle allows:
//! the durable base plus some subset of the pending batches, each whole, applied in their original
//! order. Every reopen of the store is a durability barrier, after a clean teardown or a kill: no
//! batch pending before the reopen is pending once it completes.
//! Then §2.4's third oracle step: every store call the node made after the cut is applied to the
//! reopened store and to Core's model of the recovered rows, each answer compared, and the rows and
//! caches compared at the end. The oracle decides membership without enumerating subsets; it and
//! the op selection are tested here too.

#[allow(dead_code)]
mod lk1b_is;

use std::collections::{BTreeMap, BTreeSet};

use lk1b_is::{Fixture, Op, Rows};

/// One IS `KvBatch`: puts (`Some`) and deletes (`None`), applied together.
type Batch = Vec<(Vec<u8>, Option<Vec<u8>>)>;

/// The scenarios whose IS commits the matrix cuts at.
const MATRIX_SCENARIOS: &[&str] = &["a", "b", "c", "c'", "d", "e", "f"];

fn by_key(rows: &Rows) -> BTreeMap<&[u8], &[u8]> {
    rows.iter().map(|(k, v)| (k.as_slice(), v.as_slice())).collect()
}

/// What a batch leaves for each key it touches: its last put (`Some`) or a delete (`None`).
fn effect(batch: &Batch) -> BTreeMap<&[u8], Option<&[u8]>> {
    batch.iter().map(|(k, v)| (k.as_slice(), v.as_deref())).collect()
}

/// Whether `recovered` is a state §2.4 allows: `base` with some subset of `pending` applied, each
/// batch whole and in its original order.
///
/// Backwards over the batches, each is taken when every key it touches that no later taken batch
/// decided already holds the batch's value in `recovered`; untouched keys must hold `base`'s.
/// Taking such a batch never loses a solution: if a subset without it works, the subset with it
/// gives the same rows (the batch only decides keys whose recovered value it already has).
fn reachable(base: &Rows, pending: &[Batch], recovered: &Rows) -> bool {
    let recovered = by_key(recovered);
    let base = by_key(base);
    let mut decided = BTreeSet::new();
    for batch in pending.iter().rev() {
        let eff = effect(batch);
        if eff.iter().all(|(k, v)| decided.contains(k) || recovered.get(k).copied() == *v) {
            decided.extend(eff.into_keys());
        }
    }
    base.keys()
        .chain(recovered.keys())
        .filter(|k| !decided.contains(*k))
        .all(|k| base.get(k) == recovered.get(k))
}

/// Every state §2.4 allows, by enumeration: the reference [`reachable`] is checked against.
fn allowed_recovered(base: &Rows, pending: &[Batch]) -> BTreeSet<Rows> {
    assert!(pending.len() < 16, "{} pending batches", pending.len());
    (0u32..1 << pending.len())
        .map(|mask| {
            let mut map: BTreeMap<_, _> = base.iter().cloned().collect();
            for (i, batch) in pending.iter().enumerate() {
                if mask & (1 << i) == 0 {
                    continue;
                }
                for (k, v) in batch {
                    match v {
                        Some(v) => map.insert(k.clone(), v.clone()),
                        None => map.remove(k),
                    };
                }
            }
            map.into_iter().collect()
        })
        .collect()
}

/// The steps of scenarios (a)–(f) whose op commits IS batches: every op but a restart and a spork.
/// The end-to-end test cuts at each commit of each.
fn matrix_points(fixture: &Fixture) -> Vec<usize> {
    fixture
        .steps
        .iter()
        .filter(|s| MATRIX_SCENARIOS.contains(&s.scenario.as_str()))
        .filter(|s| !matches!(s.op, Op::Restart { .. } | Op::Spork { .. }))
        .map(|s| s.step)
        .collect()
}

fn row(k: &[u8], v: &[u8]) -> (Vec<u8>, Vec<u8>) {
    (k.to_vec(), v.to_vec())
}

#[test]
fn no_pending_batches_leaves_the_base() {
    let base = vec![row(b"\x04is_v", b"\x01")];
    assert_eq!(allowed_recovered(&base, &[]), BTreeSet::from([base]));
}

#[test]
fn every_subset_in_order_and_whole() {
    let v = row(b"\x04is_v", b"\x01");
    let base = vec![v.clone()];
    let lock = b"\x04is_i\x11".to_vec();
    let mined = b"\x04is_m\x11".to_vec();
    // b1 writes a lock and its mined mark, b2 overwrites the lock, b3 removes both.
    let b1: Batch = vec![(lock.clone(), Some(vec![1])), (mined.clone(), Some(vec![]))];
    let b2: Batch = vec![(lock.clone(), Some(vec![2]))];
    let b3: Batch = vec![(lock.clone(), None), (mined.clone(), None)];
    let pending = [b1, b2, b3];
    let with = |l: Option<u8>, m: bool| {
        let mut r = vec![v.clone()];
        r.extend(l.map(|l| (lock.clone(), vec![l])));
        if m {
            r.push((mined.clone(), vec![]));
        }
        r.sort();
        r
    };
    let want = BTreeSet::from([
        with(None, false),    // {}, {b3}, {b1,b3}, {b2,b3}, {b1,b2,b3}
        with(Some(1), true),  // {b1}
        with(Some(2), false), // {b2}
        with(Some(2), true),  // {b1,b2}
    ]);
    assert_eq!(allowed_recovered(&base, &pending), want);
    for state in &want {
        assert!(reachable(&base, &pending, state), "{state:?}");
    }
    // A torn b1 (the lock without its mined mark) and a lost base row are not reachable.
    assert!(!reachable(&base, &pending, &with(Some(1), false)));
    assert!(!reachable(&base, &pending, &vec![(mined.clone(), vec![])]));
    // Order: b1 then b2 reaches lock 2 with the mark; b2 then b1 cannot (b1 rewrites lock 1).
    assert!(reachable(&base, &pending[..2], &with(Some(2), true)));
    assert!(!reachable(&base, &[pending[1].clone(), pending[0].clone()], &with(Some(2), true)));
}

/// `reachable` agrees with enumeration on every state over a small key and value space, for
/// pseudo-random batch lists of up to 6 batches.
#[test]
fn reachable_matches_enumeration() {
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = |n: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % n
    };
    let key = |i: u64| vec![b'k', i as u8];
    for _ in 0..300 {
        let base: Rows = (0..3).filter(|_| next(2) == 0).map(|i| (key(i), vec![0])).collect();
        let pending: Vec<Batch> = (0..next(7))
            .map(|_| {
                (0..1 + next(3))
                    .map(|_| (key(next(3)), (next(3) != 0).then(|| vec![next(2) as u8 + 1])))
                    .collect()
            })
            .collect();
        let allowed = allowed_recovered(&base, &pending);
        // Every state over keys k0..k2 with values absent, 0, 1 or 2.
        for code in 0..4u32.pow(3) {
            let state: Rows = (0..3u64)
                .filter_map(|i| match (code / 4u32.pow(i as u32)) % 4 {
                    0 => None,
                    v => Some((key(i), vec![(v - 1) as u8])),
                })
                .collect();
            assert_eq!(
                reachable(&base, &pending, &state),
                allowed.contains(&state),
                "base {base:?} pending {pending:?} state {state:?}"
            );
        }
    }
}

#[test]
fn matrix_scenarios_are_a_to_f() {
    assert_eq!(MATRIX_SCENARIOS, ["a", "b", "c", "c'", "d", "e", "f"]);
}

#[test]
fn matrix_points_on_the_sample() {
    let fixture = lk1b_is::sample();
    // The setup spork and the restart are not cut points; (c)'s lock, block and CL are.
    assert_eq!(matrix_points(&fixture), [2, 3, 4]);
}

#[test]
fn matrix_points_on_the_main_capture() {
    let fixture = lk1b_is::capture("main");
    let points = matrix_points(&fixture);
    for scenario in MATRIX_SCENARIOS {
        assert!(
            points.iter().any(|&p| fixture.steps[p - 1].scenario == *scenario),
            "no cut point in ({scenario})"
        );
    }
}

/// The end-to-end matrix on NodeSut.
#[cfg(feature = "test-support")]
mod power_loss {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Condvar, Mutex, OnceLock};
    use std::thread::{self, ThreadId};

    use dash_kv::KvEngine;
    use dash_kv::faulty::{
        AllSurvive, CrashImage, CrashKind, Decider, EngineCut, FaultyEngine, SeededDecider, Var,
        World, WorldConfig,
    };
    use dashd_locks::is::db::core_model::Model;
    use dashd_locks::is::db::{IS, IsDb, SCHEMA};
    use dashd_locks::is::seams::{IsOverlay, IsStore, StageId};
    use dashd_node::sut::Harness;

    use super::*;
    use crate::lk1b_is::node::NodeSide;
    use crate::lk1b_is::store::{Call, Tap, TapHost};
    use crate::lk1b_is::{IsSide, Observed};

    /// The node's store calls, one at a time and logged in the order they ran. A call off the
    /// attempt's overlay waits here while the attempt holds stages, as the IS gate would park it,
    /// so the log is an order the store ran them in. `resume` is the log position after the call
    /// in progress: where a cut taken during it continues. Each reopen of `engine` is recorded
    /// with the batches pending as it began and as it completed.
    #[derive(Default)]
    struct Recorder {
        serial: Mutex<Serial>,
        wake: Condvar,
        resume: AtomicUsize,
        engine: OnceLock<Arc<FaultyEngine>>,
        reopens: Mutex<Vec<Reopen>>,
    }

    /// One reopen: its log position, whether it wiped, and the batches pending before it and
    /// after it.
    struct Reopen {
        at: usize,
        wipe: bool,
        before: Vec<u64>,
        after: Vec<u64>,
    }

    #[derive(Default)]
    struct Serial {
        log: Vec<Call>,
        stages: usize,
        stager: Option<ThreadId>,
    }

    impl Tap for Recorder {
        fn around(&self, call: Call, inner: &mut dyn FnMut(&mut Call)) {
            let me = thread::current().id();
            let waits = !call.attempt_side();
            let serial = self.serial.lock().unwrap();
            let mut serial = self
                .wake
                .wait_while(serial, |s| waits && s.stages > 0 && s.stager != Some(me))
                .unwrap();
            self.resume.store(serial.log.len() + 1, Ordering::SeqCst);
            let pending = || self.engine.get().map(|e| e.pending_ids()).unwrap_or_default();
            let wipe = match call {
                Call::Open {
                    wipe,
                } => Some(wipe),
                _ => None,
            };
            let before = wipe.map(|_| pending());
            let mut call = call;
            inner(&mut call);
            if let (Some(wipe), Some(before)) = (wipe, before) {
                self.reopens.lock().unwrap().push(Reopen {
                    at: serial.log.len(),
                    wipe,
                    before,
                    after: pending(),
                });
            }
            match &call {
                Call::Stage {
                    id: Some(_),
                    ..
                } => {
                    serial.stages += 1;
                    serial.stager = Some(me);
                }
                Call::Commit {
                    ok: Some(true),
                    ..
                } => serial.stages -= 1,
                Call::Commit {
                    ..
                }
                | Call::Discard => serial.stages = 0,
                _ => {}
            }
            serial.log.push(call);
            drop(serial);
            self.wake.notify_all();
        }
    }

    fn engine_rows<E: KvEngine>(engine: &E) -> Rows {
        engine.iter_prefix(IS, &[]).map(Result::unwrap).collect()
    }

    fn must<T>(result: dash_kv::Result<T>, at: &dyn std::fmt::Display) -> T {
        result.unwrap_or_else(|e| panic!("{at}: {e}"))
    }

    /// Where a replayed call is, written only when a check fails: the cut, the call's position
    /// in the log and the call.
    struct At<'a>(&'a str, usize, &'a Call);

    impl std::fmt::Display for At<'_> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}, call {} {:?}", self.0, self.1, self.2)
        }
    }

    /// The reopened store and Core's model of it, given the same calls.
    struct Replay<E: KvEngine> {
        engine: Arc<E>,
        db: IsDb<E>,
        model: Model,
        overlay: IsOverlay,
        /// The run's stage ids and the replay's, for the stages not yet committed.
        staged: Vec<(StageId, StageId)>,
    }

    impl<E: KvEngine + 'static> Replay<E> {
        /// The store reopened on `engine` as a load opens it, and Core's model of the rows it
        /// then holds, with empty caches and `best_confirmed_height = 0` (§2.4 oracle step 2).
        fn reopen(engine: &Arc<E>, at: &str) -> Self {
            let db = must(IsDb::open(Arc::clone(engine)), &at);
            Replay {
                engine: Arc::clone(engine),
                db,
                model: Model::reopened(engine_rows(&**engine).into_iter().collect()),
                overlay: IsOverlay::default(),
                staged: Vec::new(),
            }
        }

        fn stage(&mut self, call: &Call, at: &dyn std::fmt::Display) {
            let Call::Stage {
                hash,
                root,
                height,
                id: Some(id),
            } = call
            else {
                panic!("{at}: a stage that failed in the run");
            };
            let staged = self.db.stage_remove_chained(&mut self.overlay, hash, root, *height);
            let (removal, new) = must(staged, at);
            assert_eq!(removal, self.model.chained(*hash, root, *height), "{at}");
            self.staged.push((*id, new));
        }

        /// `call` on both sides: a read's answers must be equal.
        fn apply(&mut self, call: &Call, at: &dyn std::fmt::Display) {
            let (db, model) = (&self.db, &mut self.model);
            match call {
                Call::Open {
                    wipe,
                } => {
                    assert!(self.staged.is_empty(), "{at}: a load with stages pending");
                    self.db = must(IsDb::open_wiping(Arc::clone(&self.engine), *wipe), at);
                    if *wipe {
                        self.model = Model::default();
                    } else {
                        self.model.reopen();
                    }
                }
                Call::ConflictingLock(tx) => {
                    assert_eq!(
                        must(db.conflicting_lock(tx), at),
                        model.conflicting_lock(tx),
                        "{at}"
                    )
                }
                Call::ConflictingLockIn(tx) => {
                    let found = must(db.conflicting_lock_in(&self.overlay, tx), at);
                    assert_eq!(found, model.conflicting_lock(tx), "{at}");
                }
                Call::ByHash(hash, cached) => {
                    let found = must(db.by_hash(hash, *cached), at);
                    assert_eq!(found, model.by_hash(hash, *cached), "{at}");
                }
                Call::ByTxid(txid) => {
                    assert_eq!(must(db.by_txid(txid), at), model.by_txid(txid), "{at}")
                }
                Call::ByInput(input) => {
                    assert_eq!(must(db.by_input(input), at), model.by_input(input), "{at}")
                }
                Call::Known(hash) => {
                    assert_eq!(must(db.known(hash), at), model.known(hash), "{at}")
                }
                Call::WriteNew(hash, lock) => {
                    must(db.write_new(hash, lock), at);
                    model.write(*hash, lock);
                }
                Call::WriteMined(hash, height) => {
                    must(db.write_mined(hash, *height), at);
                    model.write_mined(*hash, *height);
                }
                Call::BlockConnected(txs, height) => {
                    must(db.block_connected(txs, *height), at);
                    model.block(txs, *height, true);
                }
                Call::BlockDisconnected(txs, height) => {
                    must(db.block_disconnected(txs, *height), at);
                    model.block(txs, *height, false);
                }
                Call::RemoveConfirmed(height) => {
                    let removed = must(db.remove_confirmed(*height), at);
                    assert_eq!(removed, model.confirm(*height), "{at}");
                }
                Call::RemoveArchived(height) => {
                    must(db.remove_archived(*height), at);
                    model.prune(*height);
                }
                Call::Stage {
                    ..
                } => self.stage(call, at),
                Call::Commit {
                    stage,
                    ok,
                } => {
                    assert_eq!(*ok, Some(true), "{at}: the run's commit failed");
                    let index = self.staged.iter().position(|(run, _)| run == stage);
                    let (_, new) = self.staged.remove(index.expect("a staged commit"));
                    must(self.db.commit_staged(&mut self.overlay, new), at);
                }
                Call::Discard => {
                    assert!(self.staged.is_empty(), "{at}: a discard with stages pending");
                    self.db.discard_staged(&mut self.overlay);
                }
                Call::RemoveChained {
                    hash,
                    root,
                    height,
                } => {
                    let removal = must(db.remove_chained(hash, root, *height), at);
                    assert_eq!(removal, model.chained(*hash, root, *height), "{at}");
                }
            }
        }
    }

    /// The calls the replays must apply between them, so that step 3 can't pass with nothing
    /// compared.
    const REPLAYED: [&str; 8] = [
        "Open",
        "Known",
        "WriteNew",
        "BlockConnected",
        "RemoveConfirmed",
        "RemoveArchived",
        "Stage",
        "Commit",
    ];

    /// §2.4 oracle step 3 on `engine`, booted from a cut: every call `log` holds from `resume` on,
    /// applied to the reopened store and to Core's model. A stage made before the cut and
    /// committed after it is staged first, as Core's inline removal writes it after the cut.
    /// Every answer must be the model's, and so must the rows and caches at the end. Each call
    /// applied is counted in `applied` by its name.
    fn replay<E: KvEngine + 'static>(
        engine: &Arc<E>,
        log: &[Call],
        resume: usize,
        at: &str,
        applied: &mut BTreeMap<&'static str, usize>,
    ) {
        let mut replay = Replay::reopen(engine, at);
        let mut pending = Vec::new();
        for (n, call) in log[..resume].iter().enumerate() {
            match call {
                Call::Stage {
                    id: Some(_),
                    ..
                } => pending.push(n),
                Call::Commit {
                    stage,
                    ..
                } => pending.retain(
                    |&i| !matches!(&log[i], Call::Stage { id: Some(id), .. } if id == stage),
                ),
                Call::Discard => pending.clear(),
                _ => {}
            }
        }
        for n in pending {
            replay.stage(&log[n], &At(at, n, &log[n]));
        }
        for (n, call) in log.iter().enumerate().skip(resume) {
            replay.apply(call, &At(at, n, call));
            *applied.entry(call.name()).or_default() += 1;
        }
        let rows: BTreeMap<_, _> = engine_rows(&*replay.engine).into_iter().collect();
        assert_eq!(rows, replay.model.rows, "{at}: rows after the replay");
        let caches = replay.db.cache_snapshot();
        assert_eq!(caches, replay.model.snapshot(), "{at}: caches after the replay");
    }

    /// What [`deciders`] names: every pending batch lost.
    struct AllLost;

    impl Decider for AllLost {
        fn choose(&mut self, var: &Var) -> u32 {
            var.domain - 1
        }
    }

    /// The power losses each cut is booted with: everything survives, nothing does, and seeded
    /// subsets.
    fn deciders() -> Vec<(String, Box<dyn Decider>)> {
        let mut out: Vec<(String, Box<dyn Decider>)> = vec![
            ("all survive".into(), Box::new(AllSurvive)),
            ("all lost".into(), Box::new(AllLost)),
        ];
        out.extend((1..=4).map(|seed| {
            (format!("seed {seed}"), Box::new(SeededDecider::new(seed)) as Box<dyn Decider>)
        }));
        out
    }

    /// A cut's IS rows and batches (the other column families hold the schema record only).
    fn is_cut(cut: &EngineCut) -> (Rows, Vec<Batch>) {
        let base =
            cut.base.iter().filter(|(cf, ..)| *cf == IS).map(|(_, k, v)| row(k, v)).collect();
        let pending = cut
            .pending
            .iter()
            .map(|b| b.iter().filter(|(cf, ..)| *cf == IS).map(|(_, k, v)| (k.clone(), v.clone())))
            .map(Iterator::collect::<Batch>)
            .filter(|b| !b.is_empty())
            .collect();
        (base, pending)
    }

    /// `base` with every batch of `pending` applied.
    fn applied(base: &Rows, pending: &[Batch]) -> Rows {
        let mut map: BTreeMap<_, _> = base.iter().cloned().collect();
        for (k, v) in pending.iter().flatten() {
            match v {
                Some(v) => map.insert(k.clone(), v.clone()),
                None => map.remove(k),
            };
        }
        map.into_iter().collect()
    }

    /// NodeSut over the FaultyEngine, the matrix's points armed while their op runs.
    struct Cutting {
        side: NodeSide,
        /// Per step, oldest first: its number when it is a matrix point, else 0.
        points: Vec<usize>,
        next: usize,
        armed: Arc<AtomicUsize>,
    }

    impl IsSide for Cutting {
        fn set_time(&mut self, mocktime: i64) {
            self.side.set_time(mocktime);
        }

        fn apply(&mut self, op: &Op) -> Result<Observed, String> {
            let point = self.points[self.next];
            self.next += 1;
            self.armed.store(point, Ordering::SeqCst);
            let observed = self.side.apply(op);
            self.armed.store(0, Ordering::SeqCst);
            observed
        }
    }

    #[test]
    fn power_loss_at_every_is_commit() {
        let fixture = lk1b_is::capture("main");
        let points = matrix_points(&fixture);
        assert!(!points.is_empty());
        let armed = Arc::new(AtomicUsize::new(0));
        let recorder = Arc::new(Recorder::default());
        let cuts: Arc<Mutex<Vec<(usize, usize, CrashImage)>>> = Arc::default();
        let world = World::new(WorldConfig {
            on_event: Some(Box::new({
                let armed = Arc::clone(&armed);
                let cuts = Arc::clone(&cuts);
                let recorder = Arc::clone(&recorder);
                move |world: &World, _| {
                    let step = armed.load(Ordering::SeqCst);
                    if step != 0 {
                        let resume = recorder.resume.load(Ordering::SeqCst);
                        cuts.lock().unwrap().push((step, resume, world.capture()));
                    }
                }
            })),
            ..WorldConfig::default()
        });
        let engine = world.engine("isdb", &SCHEMA);
        assert!(recorder.engine.set(Arc::clone(&engine)).is_ok());
        let harness = Harness {
            is_engine: Some(Arc::new(TapHost::new(Arc::clone(&engine), Arc::clone(&recorder)))),
            ..Harness::default()
        };
        let mut side = Cutting {
            side: NodeSide::new(&fixture, harness).expect("the side"),
            points: fixture
                .steps
                .iter()
                .map(|s| {
                    if points.contains(&s.step) {
                        s.step
                    } else {
                        0
                    }
                })
                .collect(),
            next: 0,
            armed,
        };
        let mismatches = lk1b_is::drive(&fixture, &mut side).unwrap_or_else(|e| panic!("{e}"));
        assert!(mismatches.is_empty(), "{mismatches:#?}");
        drop(side);

        // The barrier, where each reopen completes; a kill's reopen finds batches pending.
        let reopens = std::mem::take(&mut *recorder.reopens.lock().unwrap());
        for reopen in &reopens {
            if let Some(id) = reopen.before.iter().find(|id| reopen.after.contains(id)) {
                panic!("batch {id} is pending across the reopen at call {}", reopen.at);
            }
        }
        let restarts = fixture.steps.iter().filter(|s| matches!(s.op, Op::Restart { .. })).count();
        assert_eq!(reopens.len(), restarts + 1, "the first load and each restart's");
        assert!(
            reopens.iter().any(|r| !r.wipe && !r.before.is_empty()),
            "a kill's reopen without the wipe, whose own batch would be a barrier"
        );

        let cuts = std::mem::take(&mut *cuts.lock().unwrap());
        let log = std::mem::take(&mut recorder.serial.lock().unwrap().log);
        let mut cut_scenarios = BTreeSet::new();
        let mut checked = 0;
        let mut replayed = BTreeMap::new();
        let mut last = None;
        for (step, resume, image) in &cuts {
            let cut =
                image.engine_cut("isdb").unwrap_or_else(|| panic!("step {step}: an undecided cut"));
            if last.as_ref() == Some(&cut) {
                continue;
            }
            let (base, pending) = is_cut(&cut);
            last = Some(cut);
            if pending.is_empty() {
                continue;
            }
            cut_scenarios.insert(fixture.steps[step - 1].scenario.clone());
            for (name, decider) in deciders() {
                let booted = image.boot(CrashKind::PowerLoss, decider, WorldConfig::default());
                let engine = booted.engine("isdb", &SCHEMA);
                let db = IsDb::open(Arc::clone(&engine))
                    .unwrap_or_else(|e| panic!("step {step}, {name}: reopen: {e}"));
                let rows = engine_rows(&*engine);
                drop(db);
                let at = format!("step {step}, {} pending, {name}", pending.len());
                assert!(reachable(&base, &pending, &rows), "{at}: rows {rows:?}");
                match name.as_str() {
                    "all survive" => assert_eq!(rows, applied(&base, &pending), "{at}"),
                    "all lost" => assert_eq!(rows, base, "{at}"),
                    _ => {}
                }
                checked += 1;
                replay(&engine, &log, *resume, &at, &mut replayed);
            }
        }
        eprintln!(
            "{} images, {checked} power losses checked, {} calls replayed of {}: {replayed:?}",
            cuts.len(),
            replayed.values().sum::<usize>(),
            log.len()
        );
        for name in REPLAYED {
            assert!(replayed.contains_key(name), "no {name} call replayed: {replayed:?}");
        }
        let want: BTreeSet<String> = MATRIX_SCENARIOS.iter().map(|s| s.to_string()).collect();
        assert_eq!(cut_scenarios, want, "every matrix scenario cut with batches pending");
    }
}
