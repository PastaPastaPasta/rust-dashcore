//! CP2.1 regressions (reviews/ST3a-cp2-sol.md): Sol's reproductions of CP2-1 to CP2-6, kept as
//! written so they run unchanged against 0cc7126b (where all but the determinism control fail).
//!
//! - CP2-1: `cp2_r5_*` (R5 compares file records and index metadata; prune exemptions need an
//!   eligible selection and a completed Bk/Bp).
//! - CP2-2: `cp2_two_unknowns_*` (a failure through an undecided read is re-derived).
//! - CP2-3: `cp2_recovery_fault_does_not_hit_store_check` (checker reads take no faults).
//! - CP2-4: `cp2_r7a_*` (R7a under an inner PowerLoss, and in replay).
//! - CP2-5: `cp2_r6_checks_script_provenance`.
//! - CP2-6: `cp2_boot_cache_honors_d1_enablement`.

use dashd_store::codec::ValueCodec;
use dashd_store::faulty::toy::reference::ToyRef;
use dashd_store::faulty::toy::{CHAIN_SCHEMA, FILES, INDEX, ToyChain, ToyConfig, ToyOp};
use dashd_store::faulty::{
    Abort, AllSurvive, CrashKind, EventSel, FileRef, Hash, InvariantId, Mode, Nested, Peek,
    RefExtractor, Reference, Requirement, Scenario, Sut, Trace, World, WorldConfig,
    WorldDurableView, explore, replay,
};
use dashd_store::flatfile::{BlockFileInfo, FlatFs};
use dashd_store::{Cf, CfProfile, CfSpec, Commit, DbSchema, KvBatch, KvEngine, KvRead};
use std::sync::Arc;
static SCHEMA: DbSchema = DbSchema {
    name: "probe",
    version: 1,
    cfs: &[CfSpec::new("meta", CfProfile::Ordered)],
};
const CF: Cf = Cf::new(0);
fn put(w: &World, key: &[u8], val: &[u8], c: Commit) {
    let e = w.engine("probe", &SCHEMA);
    let mut b = e.batch();
    b.put(CF, key, val);
    e.write(b, c).unwrap();
}
struct TwoUnknown;
impl RefExtractor for TwoUnknown {
    fn refs(&self, _: &str, _: Cf, k: &[u8], _: &[u8], _: &WorldDurableView) -> Vec<Requirement> {
        if k == b"loc" {
            vec![Requirement::Block {
                hash: [1; 32],
                undo: false,
            }]
        } else {
            vec![]
        }
    }
    fn resolve(&self, _: &Requirement, v: &WorldDurableView) -> Result<Vec<FileRef>, String> {
        match v.get("probe", CF, b"idx") {
            Peek::Known(Some(_)) => Ok(vec![FileRef::new("/dd/f", 0, 512)]),
            Peek::Unknown => Err("index undecided".into()),
            _ => Err("missing idx".into()),
        }
    }
}
#[test]
fn cp2_two_unknowns_do_not_report_a_false_dependency_failure() {
    let w = World::new(WorldConfig::default());
    w.set_extractor(Arc::new(TwoUnknown));
    let fs = w.fs();
    let f = std::path::Path::new("/dd/f");
    fs.write_all_at(f, 0, &[7; 512]).unwrap();
    fs.sync_data(f).unwrap();
    fs.sync_dir(std::path::Path::new("/dd")).unwrap();
    put(&w, b"idx", b"good", Commit::Buffered);
    put(&w, b"loc", b"v", Commit::Buffered);
    let w = w.capture().boot(CrashKind::PowerLoss, Box::new(AllSurvive), WorldConfig::default());
    assert!(w.d1_violations().is_empty());
    let e = w.engine("probe", &SCHEMA);
    assert!(e.get(CF, b"loc").unwrap().is_some());
    assert!(e.get(CF, b"idx").unwrap().is_some());
    w.evaluate_d1();
    eprintln!("two-unknown D1: {:?}", w.d1_violations());
    assert!(w.d1_violations().is_empty(), "a correct all-survived image is D1 clean");
}
fn toy_recovered() -> (World, ToyChain, ToyRef) {
    let cfg = ToyConfig::default();
    let w = World::new(WorldConfig::default());
    w.set_extractor(ToyChain::extractor(&cfg).unwrap());
    let mut t = ToyChain::open(&w, &cfg).unwrap();
    let mut r = ToyRef::new(&cfg);
    for op in [ToyOp::Connect(10), ToyOp::Flush] {
        t.apply(&op).unwrap();
        r.apply(&op);
    }
    let w = w.capture().boot(CrashKind::PowerLoss, Box::new(AllSurvive), WorldConfig::default());
    let t = ToyChain::open(&w, &cfg).unwrap();
    assert!(t.check(&w, &r, &w.trace()).is_ok());
    (w, t, r)
}
#[test]
fn cp2_r5_rejects_changed_file_metadata() {
    let (w, t, r) = toy_recovered();
    let e = w.engine("chainstate", &CHAIN_SCHEMA);
    let key = 0u32.to_be_bytes();
    let mut info = BlockFileInfo::decode_value(&e.get(FILES, &key).unwrap().unwrap()).unwrap();
    info.height_last += 100;
    let mut bytes = Vec::new();
    info.encode_value(&mut bytes);
    let mut b = e.batch();
    b.put(FILES, &key, &bytes);
    e.write(b, Commit::Durable).unwrap();
    let got = t.check(&w, &r, &w.trace());
    eprintln!("file metadata tamper: {got:?}, D1={:?}", w.d1_violations());
    assert!(
        got.is_err_and(|v| v.iter().any(|v| v.invariant == InvariantId::R5)),
        "R5 must preserve E's block_files values"
    );
}
#[test]
fn cp2_r5_rejects_changed_index_height() {
    let (w, t, r) = toy_recovered();
    let e = w.engine("chainstate", &CHAIN_SCHEMA);
    let key = r.active[5];
    let mut entry = e.get(INDEX, &key).unwrap().unwrap();
    entry[..4].copy_from_slice(&999u32.to_le_bytes());
    let mut b = e.batch();
    b.put(INDEX, &key, &entry);
    e.write(b, Commit::Durable).unwrap();
    let got = t.check(&w, &r, &w.trace());
    eprintln!("index height tamper: {got:?}, D1={:?}", w.d1_violations());
    assert!(
        got.is_err_and(|v| v.iter().any(|v| v.invariant == InvariantId::R5)),
        "R5 must preserve E's index metadata"
    );
}
struct NoRef;
impl Reference for NoRef {
    type Config = String;
    type Op = String;
    fn new(_: &String) -> Self {
        Self
    }
    fn apply(&mut self, _: &String) {}
}
struct Finalizer {
    p: Hash,
    w: World,
}
impl Sut for Finalizer {
    type Config = String;
    type Op = String;
    type Ref = NoRef;
    const NAME: &'static str = "cp2-finalizer";
    fn open(w: &World, c: &String) -> Result<Self, Abort> {
        let mut p = [0; 32];
        if w.epoch() > 0 {
            let e = w.engine("probe", &SCHEMA);
            let done = e.get(CF, b"done").unwrap().is_some();
            let flag = e.get(CF, b"flag").unwrap().is_some();
            if done && !flag {
                p = [9; 32];
            }
            if !done {
                put(w, b"flag", b"v", Commit::Buffered);
                if c == "ok" {
                    e.sync_wal().unwrap();
                }
                put(w, b"done", b"v", Commit::Durable);
            }
        }
        Ok(Self {
            p,
            w: w.clone(),
        })
    }
    fn apply(&mut self, _: &String) -> Result<(), Abort> {
        put(&self.w, b"baseline", b"v", Commit::Durable);
        Ok(())
    }
    fn check(
        &self,
        _: &World,
        _: &NoRef,
        _: &Trace,
    ) -> Result<(), Vec<dashd_store::faulty::Violation>> {
        Ok(())
    }
    fn projection(&self) -> Option<Hash> {
        Some(self.p)
    }
}
#[test]
fn cp2_r7a_covers_inner_power_loss() {
    for cfg in ["ok", "bug"] {
        let mut sc = Scenario::<Finalizer>::new("cp2-r7a", cfg.into(), vec![String::new()]);
        sc.window.from = EventSel::Index(1);
        sc.nested = Nested::Depth1;
        let got = explore(
            &sc,
            Mode::Exhaustive {
                max_branches_per_image: 1024,
            },
        );
        eprintln!("R7a {cfg}: {}", got.summary());
        if cfg == "ok" {
            assert!(got.is_clean());
        } else {
            assert!(
                got.found().contains(&InvariantId::R7a),
                "missing inner PowerLoss non-convergence"
            );
        }
    }
}
struct Incrementer {
    p: Hash,
    w: World,
}
impl Sut for Incrementer {
    type Config = String;
    type Op = String;
    type Ref = NoRef;
    const NAME: &'static str = "cp2-incrementer";
    fn open(w: &World, _: &String) -> Result<Self, Abort> {
        let mut p = [0; 32];
        if w.epoch() > 0 {
            let e = w.engine("probe", &SCHEMA);
            p[0] = e.get(CF, b"counter").unwrap().map_or(0, |v| v[0]) + 1;
            put(w, b"counter", &[p[0]], Commit::Durable);
        }
        Ok(Self {
            p,
            w: w.clone(),
        })
    }
    fn apply(&mut self, _: &String) -> Result<(), Abort> {
        put(&self.w, b"baseline", b"v", Commit::Durable);
        Ok(())
    }
    fn check(
        &self,
        _: &World,
        _: &NoRef,
        _: &Trace,
    ) -> Result<(), Vec<dashd_store::faulty::Violation>> {
        Ok(())
    }
    fn projection(&self) -> Option<Hash> {
        Some(self.p)
    }
}
#[test]
fn cp2_r7a_repro_replays_r7a() {
    let mut sc = Scenario::<Incrementer>::new("cp2-replay", String::new(), vec![String::new()]);
    sc.window.from = EventSel::Index(1);
    sc.kinds = vec![CrashKind::ProcessKill];
    sc.nested = Nested::Depth1;
    let got = explore(
        &sc,
        Mode::Exhaustive {
            max_branches_per_image: 1024,
        },
    );
    let f = got.first(InvariantId::R7a).unwrap();
    eprintln!("explore R7a: {}\n{}", got.summary(), f.repro.to_text());
    let v = replay::<Incrementer>(&f.repro).unwrap();
    eprintln!("replay R7a: {v:?}");
    assert!(v.iter().any(|v| v.invariant == InvariantId::R7a));
}
#[test]
fn cp2_r6_checks_script_provenance() {
    let (w, t, r) = toy_recovered();
    let mut trace = w.trace();
    for item in &mut trace.entries {
        if let dashd_store::faulty::TraceEntry::Mark {
            mark:
                dashd_store::faulty::Mark::Connected {
                    scripts,
                    ..
                },
            ..
        } = item
        {
            *scripts = dashd_store::faulty::ScriptCheck::Skipped;
        }
    }
    let got = t.check(&w, &r, &trace);
    eprintln!("script provenance tamper: {got:?}");
    assert!(
        got.is_err_and(|v| v.iter().any(|v| v.invariant == InvariantId::R6)),
        "a Full-check reference must reject Skipped connects"
    );
}
#[test]
fn cp2_boot_cache_honors_d1_enablement() {
    struct Direct;
    impl RefExtractor for Direct {
        fn refs(
            &self,
            _: &str,
            _: Cf,
            k: &[u8],
            _: &[u8],
            _: &WorldDurableView,
        ) -> Vec<Requirement> {
            if k == b"loc" {
                vec![Requirement::Bytes(FileRef::new("/dd/f", 0, 512))]
            } else {
                vec![]
            }
        }
    }
    let w = World::new(WorldConfig::default());
    w.set_extractor(Arc::new(Direct));
    let f = std::path::Path::new("/dd/f");
    let fs = w.fs();
    fs.write_all_at(f, 0, &[7; 512]).unwrap();
    fs.sync_data(f).unwrap();
    fs.sync_dir(std::path::Path::new("/dd")).unwrap();
    put(&w, b"loc", b"v", Commit::Durable);
    let image = w.capture();
    let _ = image.boot(
        CrashKind::ProcessKill,
        Box::new(AllSurvive),
        WorldConfig {
            d1: false,
            ..WorldConfig::default()
        },
    );
    let w = image.boot(CrashKind::ProcessKill, Box::new(AllSurvive), WorldConfig::default());
    w.fs().remove(f).unwrap();
    eprintln!("cache: {:?}", w.d1_violations());
    assert!(w.d1_violations().iter().any(|v| v.invariant == InvariantId::D1e));
}
#[test]
fn cp2_recovery_fault_does_not_hit_store_check() {
    let mut sc = Scenario::<ToyChain>::new(
        "cp2-read-fault",
        ToyConfig::default(),
        vec![ToyOp::Connect(10), ToyOp::Flush],
    );
    sc.window.from = EventSel::Nth {
        store: dashd_store::faulty::StoreSel::Engine("chainstate".into()),
        op: dashd_store::faulty::OpKind::Barrier,
        n: 0,
    };
    sc.recovery_faults = vec![dashd_store::faulty::Fault {
        at: EventSel::Nth {
            store: dashd_store::faulty::StoreSel::Engine("chainstate".into()),
            op: dashd_store::faulty::OpKind::Read,
            n: 0,
        },
        kind: dashd_store::faulty::FaultKind::Io,
        outcome: dashd_store::faulty::FaultOutcome::NotApplied,
    }];
    let got = explore(
        &sc,
        Mode::Exhaustive {
            max_branches_per_image: 1024,
        },
    );
    eprintln!("first recovery read fault: {}", got.summary());
    assert!(got.is_clean(), "the correct toy should sanctioned-abort on a recovery read fault");
}
#[test]
fn cp2_toy_forward_and_exploration_are_deterministic() {
    let cfg = ToyConfig {
        sub: Some(dashd_store::faulty::toy::SubConfig::synced(2)),
        ..ToyConfig::default()
    };
    let mut sc = Scenario::<ToyChain>::new(
        "cp2-determinism",
        cfg,
        "c20;f;r15:8;f".split(';').map(|s| s.parse().unwrap()).collect(),
    );
    sc.window.from = EventSel::AfterMark {
        mark: dashd_store::faulty::MarkKind::FlushBegin,
        occurrence: 1,
        n: 0,
    };
    let a = dashd_store::faulty::forward_trace(&sc);
    let b = dashd_store::faulty::forward_trace(&sc);
    assert_eq!(a, b);
    let a = explore(
        &sc,
        Mode::Exhaustive {
            max_branches_per_image: 1024,
        },
    );
    let b = explore(
        &sc,
        Mode::Exhaustive {
            max_branches_per_image: 1024,
        },
    );
    assert!(a.is_clean() && b.is_clean());
    assert_eq!(a.summary(), b.summary());
    assert_eq!(a.work, b.work);
    assert_eq!(a.per_image, b.per_image);
    eprintln!(
        "deterministic hash {:x}, {}",
        dashd_store::faulty::forward_trace(&sc).0.hash(),
        a.summary()
    );
}
#[test]
fn cp2_r5_requires_an_entitled_recovery_selection() {
    for forged_mark in [false, true] {
        let (w, t, r) = toy_recovered();
        let e = w.engine("chainstate", &CHAIN_SCHEMA);
        let mut b = e.batch();
        for id in &r.active {
            let mut entry = e.get(INDEX, id).unwrap().unwrap();
            entry[36] &= !3;
            entry[41..49].fill(0);
            b.put(INDEX, id, &entry);
        }
        if forged_mark {
            w.mark(dashd_store::faulty::Mark::PruneDecision {
                due: dashd_store::faulty::PruneDue::Manual(0),
                dry_run: vec![0],
                met_target: true,
                predicted_lock: None,
                flush_for_prune: true,
                full_flush: vec![dashd_store::faulty::FlushCause::Prune],
                new: *r.active.last().unwrap(),
                unlocked: vec![0],
            });
            w.mark(dashd_store::faulty::Mark::PruneSelected {
                flush: 99,
                files: vec![0],
                bound: 0,
                rearmed: false,
                target_unmet: false,
                lock: None,
            });
        }
        e.write(b, Commit::Durable).unwrap();
        let got = t.check(&w, &r, &w.trace());
        eprintln!("illegal prune (forged mark {forged_mark}): {got:?}");
        assert!(
            got.is_err_and(|v| v.iter().any(|v| v.invariant == InvariantId::R5)),
            "a file ending at 10 is ineligible at bound 0, regardless of a selection mark"
        );
    }
}
