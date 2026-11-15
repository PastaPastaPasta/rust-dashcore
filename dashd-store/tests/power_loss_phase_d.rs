//! Phase D review regressions (the internal review of 0cc7126b..bbf3576e).
//!
//! - `extended_unreadable_over_durable_zero_bytes`: `Extended` keeps A-PSOW over synced zero
//!   bytes (durability is judged by the durable size, not the content). Since DEC-148 (2) the
//!   mixed block may be unreadable; the synced bytes are then unavailable, never changed.
//! - `r7a_after_process_kill_then_inner_power_loss`: an outer image whose recovery read state a
//!   process kill left pending is not stabilised, so a legitimate divergence after an inner
//!   power loss is no R7a; once recovery syncs first, R7a applies and holds.
//! - `w3_rename_over_an_existing_name_can_leave_it_unbound`: W3's third outcome.

use std::path::{Path, PathBuf};

use dashd_store::faulty::{
    Abort, Adversary, CrashImage, CrashKind, Dfs, EventSel, Hash, Mode, Nested, Reference,
    Scenario, Sut, Trace, World, WorldConfig, explore,
};
use dashd_store::flatfile::FlatFs;
use dashd_store::{Cf, CfProfile, CfSpec, Commit, DbSchema, KvBatch, KvEngine, KvRead};

fn p(s: &str) -> PathBuf {
    PathBuf::from(format!("/dd/{s}"))
}

fn all_branches<T>(
    img: &CrashImage,
    kind: CrashKind,
    mk: impl Fn() -> WorldConfig,
    mut f: impl FnMut(&World) -> T,
) -> Vec<T> {
    let dfs = Dfs::new();
    let mut out = Vec::new();
    loop {
        let w = img.boot(kind, dfs.decider(), mk());
        out.push(f(&w));
        if !dfs.advance() {
            return out;
        }
    }
}

/// Q3: a synced record whose bytes in a shared unit are all zero (e.g. nLockTime = 0 just past a
/// sector boundary), and a pending append into that unit. Under A-PSOW the synced record is never
/// changed; per DEC-148 (2) the block holding it and the unsynced tail may return EIO instead.
#[test]
fn extended_unreadable_over_durable_zero_bytes() {
    let mk = || WorldConfig {
        adversary: Adversary::Extended,
        ..WorldConfig::default()
    };
    let w = World::new(mk());
    let fs = w.fs();
    fs.create(&p("f")).unwrap();
    let mut a = vec![0x11u8; 512];
    a.extend([0u8; 4]);
    fs.write_all_at(&p("f"), 0, &a).unwrap();
    fs.sync_data(&p("f")).unwrap();
    fs.sync_dir(Path::new("/dd")).unwrap();
    fs.write_all_at(&p("f"), 516, &[0x22u8; 100]).unwrap();
    let img = w.capture();
    let res = all_branches(&img, CrashKind::PowerLoss, mk, |w2| {
        let mut buf = vec![0u8; 516];
        w2.fs().read_exact_at(&p("f"), 0, &mut buf).map(|_| buf == a).map_err(|e| e.to_string())
    });
    eprintln!("branches: {res:?}");
    let eio = |r: &Result<bool, String>| r.as_ref().is_err_and(|e| e.starts_with("EIO"));
    assert!(res.iter().all(|r| r == &Ok(true) || eio(r)), "synced record changed: {res:?}");
    assert!(res.iter().any(|r| r == &Ok(true)), "synced record never readable: {res:?}");
}

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

struct NoRef;
impl Reference for NoRef {
    type Config = String;
    type Op = String;
    fn new(_: &String) -> Self {
        Self
    }
    fn apply(&mut self, _: &String) {}
}

/// Q1: recovery reads a forward-run Buffered value `x` (visible after a ProcessKill), derives its
/// result from it and stores the result Durable. "syncfirst" syncs the WAL before reading.
struct Deriver {
    p: Hash,
    w: World,
}
impl Sut for Deriver {
    type Config = String;
    type Op = String;
    type Ref = NoRef;
    const NAME: &'static str = "review-d-deriver";
    fn open(w: &World, c: &String) -> Result<Self, Abort> {
        let mut p = [0; 32];
        if w.epoch() > 0 {
            let e = w.engine("probe", &SCHEMA);
            if c == "syncfirst" {
                e.sync_wal().unwrap();
            }
            let r = match e.get(CF, b"result").unwrap() {
                Some(v) => v[0],
                None => {
                    let r = if e.get(CF, b"x").unwrap().is_some() {
                        1
                    } else {
                        2
                    };
                    put(w, b"result", &[r], Commit::Durable);
                    r
                }
            };
            p[0] = r;
        }
        Ok(Self {
            p,
            w: w.clone(),
        })
    }
    fn apply(&mut self, _: &String) -> Result<(), Abort> {
        put(&self.w, b"x", b"v", Commit::Buffered);
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
fn r7a_after_process_kill_then_inner_power_loss() {
    for cfg in ["plain", "syncfirst"] {
        let mut sc = Scenario::<Deriver>::new("review-d-r7a", cfg.into(), vec![String::new()]);
        sc.window.from = EventSel::Index(0);
        sc.kinds = vec![CrashKind::ProcessKill, CrashKind::PowerLoss];
        sc.nested = Nested::Depth1;
        let got = explore(
            &sc,
            Mode::Exhaustive {
                max_branches_per_image: 1024,
            },
        );
        eprintln!("{cfg}: {}", got.summary());
        assert!(got.is_clean(), "{cfg}: {}", got.summary());
        // DEC-162: a branch whose inner crash lost the inherited `x` after the recovery read it
        // derives 2, which the outer image crashed before its recovery, `x` lost, reproduces.
        assert!(got.r7a.converged > 0, "{cfg}: {:?}", got.r7a);
        if cfg == "plain" {
            assert!(got.r7a.witnessed_before > 0, "{cfg}: {:?}", got.r7a);
        }
    }
}

/// W3 (§6.5, erratum I26): a rename over an existing name may leave it unbound, besides keeping
/// the old or the new binding; without W3 it is atomic.
#[test]
fn w3_rename_over_an_existing_name_can_leave_it_unbound() {
    use dashd_store::faulty::WitnessSet;
    let setup = |w: &World| {
        let fs = w.fs();
        for (n, b) in [("a", 1u8), ("c", 2u8)] {
            fs.write_all_at(&p(n), 0, &[b]).unwrap();
            fs.sync_data(&p(n)).unwrap();
        }
        fs.sync_dir(Path::new("/dd")).unwrap();
        fs.rename(&p("a"), &p("c")).unwrap();
    };
    let state = |w2: &World| {
        let fs = w2.fs();
        let c = fs.len(&p("c")).unwrap().map(|_| {
            let mut b = [0u8];
            fs.read_exact_at(&p("c"), 0, &mut b).unwrap();
            b[0]
        });
        (fs.len(&p("a")).unwrap().is_some(), c)
    };
    let w = World::new(WorldConfig::default());
    setup(&w);
    let atomic: std::collections::BTreeSet<_> =
        all_branches(&w.capture(), CrashKind::PowerLoss, WorldConfig::default, state)
            .into_iter()
            .collect();
    assert_eq!(atomic, [(false, Some(1)), (true, Some(2))].into(), "atomic rename");
    let w3 = || WorldConfig {
        adversary: Adversary::Witness(WitnessSet {
            break_rename: true,
            ..WitnessSet::default()
        }),
        ..WorldConfig::default()
    };
    let w = World::new(w3());
    setup(&w);
    let broken: std::collections::BTreeSet<_> =
        all_branches(&w.capture(), CrashKind::PowerLoss, w3, state).into_iter().collect();
    assert!(
        broken.contains(&(false, None)) || broken.contains(&(true, None)),
        "unbound: {broken:?}"
    );
    assert!(broken.contains(&(true, Some(1))), "both names: {broken:?}");
}
