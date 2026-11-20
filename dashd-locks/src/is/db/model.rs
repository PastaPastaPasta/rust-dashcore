use std::collections::BTreeSet;
use std::fmt::Debug;
use std::hash::Hash;
use std::sync::Arc;

use dash_consensus::Tx;
use dash_consensus::primitives::OutPoint;
use dash_evo::locks::isdlock_hash;
use dash_kv::faulty::{
    CommitKind, CrashKind, Dfs, EventSel, Fault, FaultKind, FaultOutcome, OpKind, StoreSel, World,
    WorldConfig,
};
use dash_kv::{KvEngine, MemoryEngine};
use proptest::prelude::*;

use super::core_model::{Cache, Changes, Model, Rows, apply, prefix};
use super::lru::Lru;
use super::tests::{input, join_within, lock, rows, spending, transaction, wait_until};
use super::{IsDb, OverlayView, ReadView, SCHEMA};
use crate::is::seams::{IsOverlay, IsStore};
use crate::is::{IsLockHash, LockedTx};

fn fixtures() -> Vec<LockedTx> {
    (0..8)
        .map(|i| {
            LockedTx::new(
                1,
                vec![
                    OutPoint::new(
                        transaction(i / 2).txid(),
                        if i % 2 == 0 {
                            1
                        } else {
                            256
                        },
                    ),
                    input(90, i % 3),
                ],
                transaction(i).txid(),
                Default::default(),
                [0; 96].into(),
            )
        })
        .collect()
}

fn compare_cache<K: Clone + Eq + Hash + Debug, V: Clone + Eq + Debug>(
    actual: &Lru<K, V>,
    model: &Cache<K, V>,
) {
    let mut entries: Vec<_> = actual.entries().iter().collect();
    entries.sort_by_key(|(_, (_, time))| *time);
    let entries: Vec<_> = entries.into_iter().map(|(k, (v, _))| (k.clone(), v.clone())).collect();
    assert_eq!(entries, model.0);
}

fn compare_state<E: KvEngine>(db: &IsDb<E>, model: &Model) {
    assert_eq!(rows(&*db.engine), model.rows);
    let state = db.state.lock();
    assert_eq!(state.best_confirmed_height, model.confirmed);
    compare_cache(&state.locks, &model.hashes);
    compare_cache(&state.txids, &model.txids);
    compare_cache(&state.inputs, &model.inputs);
}

fn compare_lookups<E: KvEngine>(db: &IsDb<E>, model: &mut Model, locks: &[LockedTx]) {
    compare_state(db, model);
    for lock in locks {
        let hash = isdlock_hash(lock);
        assert_eq!(db.by_hash(&hash, true).unwrap(), model.by_hash(&hash, true));
        assert_eq!(db.hash_by_txid(&lock.txid()).unwrap(), model.hash_by_txid(&lock.txid()));
        assert_eq!(db.by_txid(&lock.txid()).unwrap(), model.by_txid(&lock.txid()));
        for input in lock.inputs() {
            assert_eq!(db.by_input(input).unwrap(), model.by_input(input));
        }
        assert_eq!(db.known(&hash).unwrap(), model.known(&hash));
        assert_eq!(db.by_parent(&lock.txid()).unwrap(), model.children(&lock.txid()));
        assert_eq!(db.by_hash(&hash, false).unwrap(), model.by_hash(&hash, false));
    }
    for hash in [[0; 32], [0xff; 32]] {
        assert_eq!(db.by_hash(&hash, true).unwrap(), model.by_hash(&hash, true));
        assert_eq!(db.known(&hash).unwrap(), model.known(&hash));
    }
    let count = model.rows.keys().filter(|k| k.starts_with(&prefix("is_i"))).count();
    assert_eq!(db.count().unwrap(), count);
    compare_state(db, model);
}

fn run<E: KvEngine>(db: &mut IsDb<E>, model: &mut Model, locks: &[LockedTx], op: (u8, u8, i8)) {
    let (kind, index, height) = op;
    let lock = &locks[usize::from(index) % locks.len()];
    let hash = isdlock_hash(lock);
    let height = i32::from(height);
    match kind % 13 {
        0 | 1 => {
            db.write_new(&hash, lock).unwrap();
            model.write(hash, lock);
        }
        2 => {
            db.write_mined(&hash, height).unwrap();
            model.write_mined(hash, height);
        }
        3 => assert_eq!(db.remove_confirmed(height).unwrap(), model.confirm(height)),
        4 => {
            db.remove_archived(height).unwrap();
            model.prune(height);
        }
        5 => assert_eq!(
            db.remove_chained(&hash, lock, height).unwrap(),
            model.chained(hash, lock, height)
        ),
        6 | 7 => {
            let txs = [transaction(u32::from(index) % locks.len() as u32)];
            if kind % 13 == 6 {
                db.block_connected(&txs, height).unwrap();
                model.block(&txs, height, true);
            } else {
                db.block_disconnected(&txs, height).unwrap();
                model.block(&txs, height, false);
            }
        }
        8 => assert_eq!(
            db.by_hash(&hash, index % 2 == 0).unwrap(),
            model.by_hash(&hash, index % 2 == 0)
        ),
        9 => assert_eq!(db.by_txid(&lock.txid()).unwrap(), model.by_txid(&lock.txid())),
        10 => {
            assert_eq!(db.by_input(&lock.inputs()[0]).unwrap(), model.by_input(&lock.inputs()[0]))
        }
        11 => assert_eq!(db.known(&hash).unwrap(), model.known(&hash)),
        _ => {
            *db = IsDb::open(db.engine.clone()).unwrap();
            model.reopen();
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]
    #[test]
    fn db_and_cache_match_core_model(ops in prop::collection::vec((0..13u8, any::<u8>(), -2..24i8), 0..100)) {
        let engine = Arc::new(MemoryEngine::new(&SCHEMA));
        let mut db = IsDb::open(engine).unwrap();
        let mut model = Model::default();
        let locks = fixtures();
        for op in ops {
            run(&mut db, &mut model, &locks, op);
            compare_lookups(&db, &mut model, &locks);
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(16))]
    #[test]
    fn truncation_matches_ordered_reference_cache(ops in prop::collection::vec((any::<bool>(), 0..20_002u32), 1..80)) {
        let mut cache = Lru::default();
        for key in 0..20_000 {
            cache.insert(key, key);
        }
        let mut model = Cache((0..20_000).map(|key| (key, key)).collect());
        for (insert, key) in ops {
            if insert {
                cache.insert(key, key);
                model.put(key, key);
            } else {
                assert_eq!(cache.get(&key), model.get(&key));
            }
            compare_cache(&cache, &model);
        }
        cache.insert(20_002, 20_002);
        model.put(20_002, 20_002);
        compare_cache(&cache, &model);
    }
}

fn overlay_rows<E: KvEngine>(db: &IsDb<E>, overlay: &IsOverlay) -> Rows {
    let view = OverlayView {
        engine: &*db.engine,
        overlay,
    };
    view.iter_prefix_from(&[], &[]).unwrap().into_iter().collect()
}

fn compare_caches<E: KvEngine>(db: &IsDb<E>, model: &Model) {
    let state = db.state.lock();
    compare_cache(&state.locks, &model.hashes);
    compare_cache(&state.txids, &model.txids);
    compare_cache(&state.inputs, &model.inputs);
}

type Attempt = (Vec<(u8, u8, i8)>, Vec<(u8, u8)>, i8);

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    #[test]
    fn staged_commits_match_core_inline_batches(
        attempts in prop::collection::vec(
            (
                prop::collection::vec((0..13u8, any::<u8>(), -2..24i8), 0..8),
                prop::collection::vec((any::<u8>(), any::<u8>()), 1..=3),
                0..24i8,
            ),
            1..10,
        )
    ) {
        let attempts: Vec<Attempt> = attempts;
        let engine = Arc::new(MemoryEngine::new(&SCHEMA));
        let mut db = IsDb::open(engine).unwrap();
        let mut model = Model::default();
        let locks = fixtures();
        for index in (0..locks.len() as u8).step_by(2) {
            run(&mut db, &mut model, &locks, (0, index, 0));
        }
        for (ops, stages, height) in attempts {
            for op in ops {
                run(&mut db, &mut model, &locks, op);
            }
            let height = i32::from(height);
            let committed = rows(&*db.engine);
            let mut overlay = IsOverlay::default();
            let mut ids = Vec::new();
            for (root_pick, read_pick) in stages {
                // Stored or not: a root the store no longer holds takes DEC-1631's cache rule.
                let root = &locks[usize::from(root_pick) % locks.len()];
                let hash = isdlock_hash(root);
                let (removal, id) = db.stage_remove_chained(&mut overlay, &hash, root, height).unwrap();
                prop_assert_eq!(removal, model.chained(hash, root, height));
                ids.push(id);
                let read = &locks[usize::from(read_pick) % locks.len()];
                for input in read.inputs() {
                    let tx = spending(*input, 500 + u32::from(read_pick));
                    prop_assert_eq!(
                        db.conflicting_lock_in(&overlay, &tx).unwrap(),
                        model.conflicting_lock(&tx)
                    );
                }
                prop_assert_eq!(overlay_rows(&db, &overlay), model.rows.clone());
                prop_assert_eq!(rows(&*db.engine), committed.clone());
                compare_caches(&db, &model);
            }
            for id in ids {
                db.commit_staged(&mut overlay, id).unwrap();
            }
            compare_lookups(&db, &mut model, &locks);
        }
    }
}

#[test]
fn replay_is_row_idempotent_and_caches_follow_the_replayed_model() {
    for op in [(0, 0, 0), (2, 0, 9), (3, 0, 9), (4, 0, 9), (5, 0, 9), (6, 0, 9), (7, 0, 9)] {
        let engine = Arc::new(MemoryEngine::new(&SCHEMA));
        let mut db = IsDb::open(engine).unwrap();
        let mut model = Model::default();
        let locks = fixtures();
        for index in 0..8 {
            if op.0 == 0 && index == 0 {
                continue;
            }
            run(&mut db, &mut model, &locks, (0, index, 0));
            run(&mut db, &mut model, &locks, (2, index, 9));
        }
        if op.0 == 4 {
            run(&mut db, &mut model, &locks, (3, 0, 9));
        }
        if op.0 == 6 {
            run(&mut db, &mut model, &locks, (7, 0, 9));
        }
        run(&mut db, &mut model, &locks, op);
        let once = rows(&*db.engine);
        run(&mut db, &mut model, &locks, op);
        assert_eq!(rows(&*db.engine), once);
        compare_lookups(&db, &mut model, &locks);
    }
}

fn allowed_subsets(base: &Rows, batches: &[Changes]) -> BTreeSet<Rows> {
    (0..1 << batches.len())
        .map(|mask| {
            let mut state = base.clone();
            for (index, batch) in batches.iter().enumerate() {
                if mask & (1 << index) != 0 {
                    apply(&mut state, batch);
                }
            }
            state
        })
        .collect()
}

#[test]
fn power_loss_recovers_any_atomic_subset_after_the_durable_barrier() {
    let locks = fixtures();
    let operations = [(0, 1, 0), (2, 1, 5), (3, 1, 5), (5, 0, 6), (4, 0, 6)];
    for cut in 0..=operations.len() {
        let dfs = Dfs::new();
        let mut recovered = BTreeSet::new();
        let mut allowed = BTreeSet::new();
        loop {
            let world = World::new(WorldConfig::default());
            let engine = world.engine("is", &SCHEMA);
            let mut db = IsDb::open(engine.clone()).unwrap();
            let mut model = Model::default();
            run(&mut db, &mut model, &locks, (0, 0, 0));
            engine.sync_wal().unwrap();
            let base = model.rows.clone();
            model.journal.clear();
            for &op in &operations[..cut] {
                run(&mut db, &mut model, &locks, op);
            }
            allowed.extend(allowed_subsets(&base, &model.journal));
            world.crash_in_place(CrashKind::PowerLoss, dfs.decider());
            drop(db);
            let reopened = IsDb::open(engine.clone()).unwrap();
            let state = rows(&*engine);
            assert!(allowed.contains(&state));
            recovered.insert(state.clone());
            let model = Model {
                rows: state,
                ..Model::default()
            };
            compare_state(&reopened, &model);
            if !dfs.advance() {
                break;
            }
        }
        assert_eq!(recovered, allowed, "every allowed subset at cut {cut} must be covered");
    }
}

#[test]
fn failing_each_buffered_commit_recovers_only_allowed_subsets() {
    let locks = fixtures();
    let operations: [(u8, u8, i8); 6] =
        [(0, 0, 0), (0, 1, 0), (2, 1, 4), (3, 1, 4), (5, 0, 5), (4, 0, 5)];
    for n in 0..operations.len() {
        for outcome in [FaultOutcome::NotApplied, FaultOutcome::Applied] {
            let dfs = Dfs::new();
            loop {
                let world = World::new(WorldConfig {
                    faults: vec![Fault {
                        at: EventSel::Nth {
                            store: StoreSel::Engine("is".into()),
                            op: OpKind::Append(CommitKind::Buffered),
                            n: n as u32,
                        },
                        kind: FaultKind::Io,
                        outcome,
                    }],
                    ..WorldConfig::default()
                });
                let engine = world.engine("is", &SCHEMA);
                let db = IsDb::open(engine.clone()).unwrap();
                let mut model = Model::default();
                let base = model.rows.clone();
                for (index, &op) in operations[..=n].iter().enumerate() {
                    let lock = &locks[usize::from(op.1)];
                    let hash = isdlock_hash(lock);
                    let result = match op.0 {
                        0 => {
                            model.write(hash, lock);
                            db.write_new(&hash, lock)
                        }
                        2 => {
                            model.write_mined(hash, i32::from(op.2));
                            db.write_mined(&hash, i32::from(op.2))
                        }
                        3 => {
                            model.confirm(i32::from(op.2));
                            db.remove_confirmed(i32::from(op.2)).map(|_| ())
                        }
                        4 => {
                            model.prune(i32::from(op.2));
                            db.remove_archived(i32::from(op.2))
                        }
                        _ => {
                            model.chained(hash, lock, i32::from(op.2));
                            db.remove_chained(&hash, lock, i32::from(op.2)).map(|_| ())
                        }
                    };
                    assert_eq!(result.is_err(), index == n);
                }
                assert!(world.fault_fired());
                if outcome == FaultOutcome::NotApplied {
                    model.journal.pop();
                }
                let allowed = allowed_subsets(&base, &model.journal);
                world.disarm_faults();
                world.crash_in_place(CrashKind::PowerLoss, dfs.decider());
                drop(db);
                let reopened = IsDb::open(engine.clone()).unwrap();
                assert!(allowed.contains(&rows(&*engine)));
                compare_state(
                    &reopened,
                    &Model {
                        rows: rows(&*engine),
                        ..Model::default()
                    },
                );
                if !dfs.advance() {
                    break;
                }
            }
        }
    }
}

/// The A/B rows: A covers x and y, B later overwrites `is_in x`, and T conflicts with A via y.
struct AbRows {
    a: LockedTx,
    b: LockedTx,
    ha: IsLockHash,
    x: OutPoint,
    t: Tx,
}

#[derive(Debug, Eq, PartialEq)]
enum Answer {
    Lock(Option<LockedTx>),
    Known(bool),
    Conflict(Option<(IsLockHash, LockedTx)>),
}

impl AbRows {
    fn new() -> Self {
        let (x, y) = (input(99, 0), input(99, 1));
        let a = lock(1, vec![x, y]);
        let b = lock(2, vec![x]);
        Self {
            ha: isdlock_hash(&a),
            a,
            b,
            x,
            t: spending(y, 40),
        }
    }

    /// Writes A then B and warms the input caches, on both the store and the model.
    fn setup() -> (Self, Arc<MemoryEngine>, Arc<IsDb<MemoryEngine>>, Model) {
        let rows = Self::new();
        let engine = Arc::new(MemoryEngine::new(&SCHEMA));
        let db = IsDb::open(engine.clone()).unwrap();
        let mut model = Model::default();
        for object in [&rows.a, &rows.b] {
            db.write_new(&isdlock_hash(object), object).unwrap();
            model.write(isdlock_hash(object), object);
        }
        for input in rows.a.inputs() {
            assert_eq!(db.by_input(input).unwrap(), model.by_input(input));
        }
        (rows, engine, Arc::new(db), model)
    }

    fn order(first: usize) -> impl Iterator<Item = usize> {
        (first..first + 5).map(|call| call % 5)
    }

    fn call_db<E: KvEngine>(&self, db: &IsDb<E>, call: usize) -> dash_kv::Result<Answer> {
        Ok(match call {
            0 => Answer::Lock(db.by_input(&self.x)?),
            1 => Answer::Lock(db.by_txid(&self.a.txid())?),
            2 => Answer::Lock(db.by_hash(&self.ha, true)?),
            3 => Answer::Known(db.known(&self.ha)?),
            _ => Answer::Conflict(db.conflicting_lock(&self.t)?),
        })
    }

    fn call_model(&self, model: &mut Model, call: usize) -> Answer {
        match call {
            0 => Answer::Lock(model.by_input(&self.x)),
            1 => Answer::Lock(model.by_txid(&self.a.txid())),
            2 => Answer::Lock(model.by_hash(&self.ha, true)),
            3 => Answer::Known(model.known(&self.ha)),
            _ => Answer::Conflict(model.conflicting_lock(&self.t)),
        }
    }

    fn sequence_db<E: KvEngine>(&self, db: &IsDb<E>, first: usize) -> dash_kv::Result<Vec<Answer>> {
        Self::order(first).map(|call| self.call_db(db, call)).collect()
    }

    fn sequence_model(&self, model: &mut Model, first: usize) -> Vec<Answer> {
        Self::order(first).map(|call| self.call_model(model, call)).collect()
    }

    fn locks(&self) -> [LockedTx; 2] {
        [self.a.clone(), self.b.clone()]
    }
}

fn spawn_sequence(
    rows: &Arc<AbRows>,
    db: &Arc<IsDb<MemoryEngine>>,
    first: usize,
) -> std::thread::JoinHandle<dash_kv::Result<Vec<Answer>>> {
    let (rows, db) = (Arc::clone(rows), Arc::clone(db));
    std::thread::spawn(move || rows.sequence_db(&db, first))
}

fn await_parked(db: &IsDb<MemoryEngine>) {
    wait_until("a caller to park on the gate", || db.gate_waiters() > 0);
}

#[test]
fn parked_reader_sequence_matches_core_inline_model() {
    for first in 0..5 {
        let (rows, _engine, db, mut model) = AbRows::setup();
        let rows = Arc::new(rows);
        let mut overlay = IsOverlay::default();
        let (removal, stage) =
            db.stage_remove_chained(&mut overlay, &rows.ha, &rows.a, 10).unwrap();
        let reader = spawn_sequence(&rows, &db, first);
        await_parked(&db);
        assert!(!reader.is_finished(), "call {first} must park on the gate");
        db.commit_staged(&mut overlay, stage).unwrap();
        let answers = join_within("the parked reader", reader).unwrap().unwrap();
        assert_eq!(removal, model.chained(rows.ha, &rows.a, 10));
        assert_eq!(answers, rows.sequence_model(&mut model, first));
        assert_eq!(db.cache_snapshot(), model.snapshot());
        compare_lookups(&db, &mut model, &rows.locks());
    }
}

#[test]
fn reader_before_the_stage_gets_the_old_rows() {
    for first in 0..5 {
        let (rows, _engine, db, mut model) = AbRows::setup();
        let rows = Arc::new(rows);
        let answers =
            join_within("the reader", spawn_sequence(&rows, &db, first)).unwrap().unwrap();
        assert_eq!(answers, rows.sequence_model(&mut model, first));
        assert!(answers.contains(&Answer::Lock(Some(rows.a.clone()))));
        let mut overlay = IsOverlay::default();
        let (removal, stage) =
            db.stage_remove_chained(&mut overlay, &rows.ha, &rows.a, 10).unwrap();
        db.commit_staged(&mut overlay, stage).unwrap();
        assert_eq!(removal, model.chained(rows.ha, &rows.a, 10));
        assert_eq!(db.cache_snapshot(), model.snapshot());
        compare_lookups(&db, &mut model, &rows.locks());
    }
}

#[test]
fn parked_reader_and_later_calls_get_the_latch_after_discard() {
    for first in 0..5 {
        let (rows, engine, db, mut model) = AbRows::setup();
        let rows = Arc::new(rows);
        let before = super::tests::rows(&*engine);
        let mut overlay = IsOverlay::default();
        db.stage_remove_chained(&mut overlay, &rows.ha, &rows.a, 10).unwrap();
        let reader = {
            let (rows, db) = (Arc::clone(&rows), Arc::clone(&db));
            std::thread::spawn(move || {
                AbRows::order(first).map(|call| rows.call_db(&db, call)).collect::<Vec<_>>()
            })
        };
        await_parked(&db);
        db.discard_staged(&mut overlay);
        let answers = join_within("the parked reader", reader).unwrap();
        assert!(answers.iter().all(Result::is_err), "{answers:?}");
        for call in 0..5 {
            assert!(rows.call_db(&db, call).is_err());
        }
        assert!(db.write_new(&isdlock_hash(&rows.b), &rows.b).is_err());
        assert!(db.write_mined(&rows.ha, 10).is_err());
        assert!(db.block_connected(std::slice::from_ref(&rows.t), 10).is_err());
        assert!(db.remove_confirmed(10).is_err());
        assert!(db.remove_archived(10).is_err());
        assert_eq!(super::tests::rows(&*engine), before);
        drop(db);
        let reopened = IsDb::open(engine.clone()).unwrap();
        assert_eq!(super::tests::rows(&*engine), before);
        model.reopen();
        compare_lookups(&reopened, &mut model, &rows.locks());
    }
}

/// The staging thread unwinds between its stage and the commit or discard (§5.3 r2f, G1): its
/// overlay drops with the stage pending, and the gate hold's `Drop` latches the store and wakes
/// the parked reader, which gets the latched error, as every later call does. Nothing was written.
#[test]
fn parked_reader_and_later_calls_get_the_latch_after_a_staging_panic() {
    for first in 0..5 {
        let (rows, engine, db, mut model) = AbRows::setup();
        let rows = Arc::new(rows);
        let before = super::tests::rows(&*engine);
        let (staged_tx, staged_rx) = std::sync::mpsc::channel();
        let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
        let stager = {
            let (rows, db) = (Arc::clone(&rows), Arc::clone(&db));
            std::thread::spawn(move || {
                let mut overlay = IsOverlay::default();
                db.stage_remove_chained(&mut overlay, &rows.ha, &rows.a, 10).unwrap();
                staged_tx.send(()).unwrap();
                go_rx.recv().unwrap();
                panic!("the staging thread unwinds with its stage pending");
            })
        };
        staged_rx.recv().unwrap();
        let reader = {
            let (rows, db) = (Arc::clone(&rows), Arc::clone(&db));
            std::thread::spawn(move || {
                AbRows::order(first).map(|call| rows.call_db(&db, call)).collect::<Vec<_>>()
            })
        };
        await_parked(&db);
        assert!(!reader.is_finished(), "call {first} must park on the gate");
        go_tx.send(()).unwrap();
        assert!(join_within("the stager", stager).is_err(), "the staging thread panicked");
        let answers = join_within("the parked reader", reader).unwrap();
        assert!(answers.iter().all(Result::is_err), "{answers:?}");
        let first = answers[0].as_ref().unwrap_err().to_string();
        assert!(first.contains(super::UNRELEASED_HOLD), "{first}");
        for call in 0..5 {
            assert!(rows.call_db(&db, call).is_err());
        }
        assert!(db.write_new(&isdlock_hash(&rows.b), &rows.b).is_err());
        assert!(db.remove_confirmed(10).is_err());
        assert_eq!(super::tests::rows(&*engine), before);
        drop(db);
        let reopened = IsDb::open(engine.clone()).unwrap();
        assert_eq!(super::tests::rows(&*engine), before);
        model.reopen();
        compare_lookups(&reopened, &mut model, &rows.locks());
    }
}

#[test]
fn dec1631_model_matches_removal_of_a_confirmed_root() {
    let engine = Arc::new(MemoryEngine::new(&SCHEMA));
    let mut db = IsDb::open(engine).unwrap();
    let mut model = Model::default();
    let locks = fixtures();
    for op in [(0, 0, 0), (0, 2, 0), (2, 0, 3), (3, 0, 3), (10, 0, 0), (9, 0, 0)] {
        run(&mut db, &mut model, &locks, op);
        compare_lookups(&db, &mut model, &locks);
    }
    let hash = isdlock_hash(&locks[0]);
    assert!(model.txids.0.iter().any(|(k, _)| *k == locks[0].txid()));
    for op in [(5, 0, 4), (5, 0, 5)] {
        run(&mut db, &mut model, &locks, op);
        compare_state(&db, &model);
        assert!(!model.hashes.0.iter().any(|(k, _)| *k == hash));
        assert!(!model.txids.0.iter().any(|(k, _)| *k == locks[0].txid()));
        assert!(!locks[0].inputs().iter().any(|i| model.inputs.0.iter().any(|(k, _)| k == i)));
        compare_lookups(&db, &mut model, &locks);
    }
}

/// The not-stored root's txid entry goes only while it names the root: a same-txid lock that
/// overwrote it keeps it, and without that lock it goes.
#[test]
fn dec1631_model_erases_a_txid_entry_only_while_it_names_the_root() {
    let txid = transaction(1).txid();
    let a = LockedTx::new(1, vec![input(91, 0)], txid, Default::default(), [0; 96].into());
    let b = LockedTx::new(1, vec![input(91, 1)], txid, Default::default(), [0; 96].into());
    let locks = [a, b];
    let hb = isdlock_hash(&locks[1]);
    for overwritten in [true, false] {
        let mut db = IsDb::open(Arc::new(MemoryEngine::new(&SCHEMA))).unwrap();
        let mut model = Model::default();
        let writes: &[(u8, u8, i8)] = if overwritten {
            &[(0, 0, 0), (0, 1, 0)]
        } else {
            &[(0, 0, 0)]
        };
        for &op in writes.iter().chain(&[(2, 0, 3), (3, 0, 3), (5, 0, 4)]) {
            run(&mut db, &mut model, &locks, op);
            compare_state(&db, &model);
        }
        let entry = model.txids.0.iter().find(|(k, _)| *k == txid).map(|(_, v)| *v);
        assert_eq!(entry, overwritten.then_some(hb), "overwritten: {overwritten}");
        compare_lookups(&db, &mut model, &locks);
    }
}

#[test]
fn dec1631_model_keeps_an_input_entry_another_lock_overwrote() {
    let engine = Arc::new(MemoryEngine::new(&SCHEMA));
    let mut db = IsDb::open(engine).unwrap();
    let mut model = Model::default();
    let locks = fixtures();
    let shared = input(90, 0);
    assert_eq!(locks[3].inputs()[1], shared);
    for op in [(0, 0, 0), (0, 3, 0), (2, 0, 3), (3, 0, 3), (5, 0, 4)] {
        run(&mut db, &mut model, &locks, op);
        compare_state(&db, &model);
    }
    let owner = isdlock_hash(&locks[3]);
    assert!(model.inputs.0.iter().any(|(k, v)| *k == shared && *v == owner));
    assert_eq!(db.by_input(&shared).unwrap(), Some(locks[3].clone()));
    assert_eq!(model.by_input(&shared), Some(locks[3].clone()));
    compare_lookups(&db, &mut model, &locks);
}
