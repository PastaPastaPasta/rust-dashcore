//! ST3a model self-tests M1–M18 (design §6.3; M0 exit 7).
//!
//! Each test drives `FaultyEngine` or `FaultyFlatFiles` directly and checks a property of the
//! model against an independent reference written here.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use dashd_store::faulty::{
    Adversary, AllSurvive, AllocateMode, CrashImage, CrashKind, Decider, Dfs, EventSel, Fault,
    FaultKind, FaultOutcome, Fixed, Granularity, OpKind, Peek, Seeded, SeededDecider, StoreSel,
    WitnessSet, World, WorldConfig,
};
use dashd_store::flatfile::{FlatFs, OpenFailed};
use dashd_store::model::{Hooks, MODEL_SCHEMA, check_with, decode_ops, pseudo_random_bytes};
use dashd_store::{Cf, CfProfile, CfSpec, Commit, DbSchema, KvBatch, KvEngine, KvRead};

static SCHEMA: DbSchema = DbSchema {
    name: "m",
    version: 1,
    cfs: &[
        CfSpec::new("meta", CfProfile::Ordered),
        CfSpec::new("a", CfProfile::Ordered),
        CfSpec::new("b", CfProfile::Ordered),
    ],
};
const A: Cf = Cf::new(1);
const B: Cf = Cf::new(2);

fn cfg() -> WorldConfig {
    WorldConfig::default()
}

fn cfg_with(adversary: Adversary) -> WorldConfig {
    WorldConfig {
        adversary,
        ..WorldConfig::default()
    }
}

fn p(s: &str) -> PathBuf {
    PathBuf::from(format!("/dd/{s}"))
}

fn put(e: &impl KvEngine, kvs: &[(Cf, &[u8], &[u8])], commit: Commit) {
    let mut b = e.batch();
    for (cf, k, v) in kvs {
        b.put(*cf, k, v);
    }
    e.write(b, commit).unwrap();
}

/// Every branch of booting `img` after `kind`, each observed by `f`.
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
        assert_eq!(dfs.mismatch(), None);
        if !dfs.advance() {
            return out;
        }
        assert!(out.len() < 100_000, "branch explosion");
    }
}

// ---------------------------------------------------------------------------------------------
// M1: a synced byte, size, binding or batch is never lost.

#[derive(Default, Clone)]
struct RefFile {
    cur: Vec<u8>,
    durable: Vec<u8>,
    dirty: bool,
}

#[derive(Default)]
struct RefWorld {
    // Engine.
    live: BTreeMap<(u16, Vec<u8>), Vec<u8>>,
    durable: BTreeMap<(u16, Vec<u8>), Vec<u8>>,
    /// Keys written since the last barrier, with every value written since.
    pending_keys: BTreeMap<(u16, Vec<u8>), Vec<Vec<u8>>>,
    // Filesystem: names → reference inode; inode contents.
    names: BTreeMap<String, usize>,
    files: Vec<RefFile>,
    pending_names: BTreeSet<String>,
}

#[test]
fn m1_synced_state_is_never_lost() {
    let names = ["f0", "f1", "f2"];
    for seed in 0..120 {
        let w = World::new(cfg());
        let e = w.engine("m", &SCHEMA);
        let fs = w.fs();
        fs.create_dir_all(&p("x")).unwrap();
        fs.sync_dir(&p("x")).unwrap();
        fs.sync_dir(Path::new("/dd")).unwrap();
        let mut r = RefWorld::default();
        let mut rng = Seeded::new(seed);
        for _ in 0..40 {
            match rng.below(10) {
                0..=2 => {
                    let n = 1 + rng.below(3);
                    let mut b = e.batch();
                    let mut ks = Vec::new();
                    for _ in 0..n {
                        let cf = if rng.chance(500) {
                            A
                        } else {
                            B
                        };
                        let k = vec![rng.below(4) as u8];
                        let v = vec![rng.below(256) as u8; 3];
                        b.put(cf, &k, &v);
                        ks.push((cf.index() as u16, k, v));
                    }
                    let durable = rng.chance(300);
                    e.write(
                        b,
                        if durable {
                            Commit::Durable
                        } else {
                            Commit::Buffered
                        },
                    )
                    .unwrap();
                    for (cf, k, v) in ks {
                        r.live.insert((cf, k.clone()), v.clone());
                        r.pending_keys.entry((cf, k)).or_default().push(v);
                    }
                    if durable {
                        r.durable = r.live.clone();
                        r.pending_keys.clear();
                    }
                }
                3 => {
                    e.sync_wal().unwrap();
                    r.durable = r.live.clone();
                    r.pending_keys.clear();
                }
                4..=5 => {
                    let name = names[rng.below(3) as usize];
                    let off = rng.below(1500);
                    let len = 1 + rng.below(1200);
                    let data: Vec<u8> = (0..len).map(|i| (seed as u8) ^ (i as u8) ^ 0x5a).collect();
                    fs.write_all_at(&p(&format!("x/{name}")), off, &data).unwrap();
                    let ino = *r.names.entry(name.into()).or_insert_with(|| {
                        r.files.push(RefFile::default());
                        r.pending_names.insert(name.into());
                        r.files.len() - 1
                    });
                    let f = &mut r.files[ino];
                    let end = (off + len) as usize;
                    if f.cur.len() < end {
                        f.cur.resize(end, 0);
                    }
                    f.cur[off as usize..end].copy_from_slice(&data);
                    f.dirty = true;
                }
                6 => {
                    let name = names[rng.below(3) as usize];
                    if let Some(&ino) = r.names.get(name) {
                        fs.sync_data(&p(&format!("x/{name}"))).unwrap();
                        let f = &mut r.files[ino];
                        f.durable = f.cur.clone();
                        f.dirty = false;
                    }
                }
                7 => {
                    fs.sync_dir(&p("x")).unwrap();
                    r.pending_names.clear();
                }
                8 => {
                    let (a, b) = (names[rng.below(3) as usize], names[rng.below(3) as usize]);
                    if a != b
                        && let Some(ino) = r.names.remove(a)
                    {
                        fs.rename(&p(&format!("x/{a}")), &p(&format!("x/{b}"))).unwrap();
                        r.names.insert(b.into(), ino);
                        r.pending_names.insert(a.into());
                        r.pending_names.insert(b.into());
                    }
                }
                _ => {
                    let name = names[rng.below(3) as usize];
                    let len = rng.below(1800) as usize;
                    if let Some(&ino) = r.names.get(name) {
                        fs.truncate(&p(&format!("x/{name}")), len as u64).unwrap();
                        let f = &mut r.files[ino];
                        f.cur.resize(len, 0);
                        f.dirty = true;
                    }
                }
            }
        }
        let img = w.capture();
        for branch in 0..4 {
            let w2 = img.boot(
                CrashKind::PowerLoss,
                Box::new(SeededDecider::new(seed * 7 + branch)),
                cfg(),
            );
            let e2 = w2.engine("m", &SCHEMA);
            for ((cf, k), v) in &r.durable {
                if !r.pending_keys.contains_key(&(*cf, k.clone())) {
                    assert_eq!(e2.get(Cf::new(*cf), k).unwrap().as_ref(), Some(v), "seed {seed}");
                }
            }
            // A key with pending writes holds its durable value or one written since.
            for ((cf, k), vals) in &r.pending_keys {
                let got = e2.get(Cf::new(*cf), k).unwrap();
                let ok = got.as_ref() == r.durable.get(&(*cf, k.clone()))
                    || got.as_ref().is_some_and(|g| vals.contains(g));
                assert!(ok, "seed {seed}: {got:?} is neither durable nor pending");
            }
            let fs2 = w2.fs();
            for (name, &ino) in &r.names {
                let f = &r.files[ino];
                if r.pending_names.contains(name) || f.dirty {
                    continue;
                }
                let got = fs2.read_all(&p(&format!("x/{name}"))).unwrap();
                assert_eq!(got.as_ref(), Some(&f.durable), "seed {seed} {name}");
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// M2: a process kill keeps every completed operation, and leaves it pending.

#[test]
fn m2_process_kill_keeps_everything_pending() {
    let w = World::new(cfg());
    let e = w.engine("m", &SCHEMA);
    let fs = w.fs();
    put(&*e, &[(A, b"k", b"durable")], Commit::Durable);
    put(&*e, &[(A, b"k", b"pending")], Commit::Buffered);
    fs.write_all_at(&p("f"), 0, &[7u8; 1000]).unwrap();
    let img = w.capture();
    let w2 = img.boot(CrashKind::ProcessKill, Box::new(AllSurvive), cfg());
    let (e2, fs2) = (w2.engine("m", &SCHEMA), w2.fs());
    assert_eq!(e2.get(A, b"k").unwrap(), Some(b"pending".to_vec()));
    assert_eq!(fs2.read_all(&p("f")).unwrap(), Some(vec![7u8; 1000]));
    assert_eq!(e2.pending(), 1, "the batch is still pending");
    let view = w2.durable_view();
    assert!(view.range_durable(&dashd_store::faulty::FileRef::new(p("f"), 0, 1000)).is_err());
    // A later power loss can still lose all of it.
    let img2 = w2.capture();
    let states = all_branches(&img2, CrashKind::PowerLoss, cfg, |w3| {
        (w3.engine("m", &SCHEMA).get(A, b"k").unwrap(), w3.fs().read_all(&p("f")).unwrap())
    });
    assert!(states.contains(&(Some(b"durable".to_vec()), None)));
    assert!(states.contains(&(Some(b"pending".to_vec()), Some(vec![7u8; 1000]))));
}

// ---------------------------------------------------------------------------------------------
// M3/M4: every byte and size of a booted image is in the `Realistic` domain (brute force).

/// A file's history since its last sync, as full snapshots, plus the sizes a crash may leave.
struct History {
    snapshots: Vec<Vec<u8>>,
    sizes: BTreeSet<u64>,
}

impl History {
    fn new(durable: Vec<u8>) -> Self {
        let mut sizes = BTreeSet::new();
        sizes.insert(durable.len() as u64);
        Self {
            snapshots: vec![durable],
            sizes,
        }
    }

    fn cur(&self) -> Vec<u8> {
        self.snapshots.last().unwrap().clone()
    }

    fn push(&mut self, v: Vec<u8>, extending_from: Option<u64>) {
        if let Some(from) = extending_from {
            let mut s = (from / 512 + 1) * 512;
            while s < v.len() as u64 {
                self.sizes.insert(s);
                s += 512;
            }
        }
        self.sizes.insert(v.len() as u64);
        self.snapshots.push(v);
    }

    fn unit_allowed(&self, i: usize, got: &[u8]) -> bool {
        self.snapshots.iter().any(|s| {
            let mut u = vec![0u8; 512];
            if s.len() > i * 512 {
                let end = s.len().min((i + 1) * 512);
                u[..end - i * 512].copy_from_slice(&s[i * 512..end]);
            }
            u[..got.len()] == *got
        })
    }
}

#[test]
fn m3_m4_bytes_and_sizes_stay_in_the_realistic_domain() {
    let mut images = 0;
    for mode in [AllocateMode::Fallocate, AllocateMode::ZeroFill] {
        for seed in 0..60 {
            let mk = || WorldConfig {
                allocate: mode,
                ..cfg()
            };
            let w = World::new(mk());
            let fs = w.fs();
            let f = p("f");
            let mut rng = Seeded::new(seed + 1000);
            let durable: Vec<u8> = (0..rng.below(1500)).map(|i| i as u8 | 1).collect();
            fs.write_all_at(&p("g"), 0, &[0x33; 700]).unwrap();
            fs.create(&f).unwrap();
            fs.write_all_at(&f, 0, &durable).unwrap();
            fs.sync_data(&f).unwrap();
            fs.sync_dir(Path::new("/dd")).unwrap();
            let mut h = History::new(durable);
            for _ in 0..3 {
                let mut v = h.cur();
                match rng.below(5) {
                    0 | 1 => {
                        let off = rng.below(1800) as usize;
                        let len = 1 + rng.below(700) as usize;
                        let data: Vec<u8> = (0..len)
                            .map(|i| (i as u8).wrapping_mul(3) ^ seed as u8 | 0x80)
                            .collect();
                        fs.write_all_at(&f, off as u64, &data).unwrap();
                        let from = v.len() as u64;
                        if v.len() < off + len {
                            v.resize(off + len, 0);
                        }
                        v[off..off + len].copy_from_slice(&data);
                        let ext = (v.len() as u64 > from).then_some(from.max(off as u64));
                        h.push(v, ext.map(|_| from));
                    }
                    2 => {
                        let n = rng.below(2000) as usize;
                        fs.truncate(&f, n as u64).unwrap();
                        v.resize(n, 0);
                        h.push(v, None);
                    }
                    3 => {
                        let off = rng.below(1500);
                        let len = 1 + rng.below(800);
                        fs.allocate(&f, off, len);
                        let end = (off + len) as usize;
                        let from = v.len() as u64;
                        if mode == AllocateMode::ZeroFill {
                            // `OsFs`'s fallback writes zeros over `[offset, end)`.
                            if v.len() < end {
                                v.resize(end, 0);
                            }
                            v[off as usize..end].fill(0);
                            h.push(v, (end as u64 > from).then_some(from));
                        } else if v.len() < end {
                            v.resize(end, 0);
                            h.push(v, None);
                        }
                    }
                    _ => {
                        fs.copy(&p("g"), &f).unwrap();
                        h.push(Vec::new(), None);
                        h.push(vec![0x33; 700], Some(0));
                    }
                }
            }
            let img = w.capture();
            images += 1;
            let mut reached_sizes = BTreeSet::new();
            let mut reached_units: BTreeSet<(usize, Vec<u8>)> = BTreeSet::new();
            for (len, bytes) in all_branches(&img, CrashKind::PowerLoss, mk, |w2| {
                let b = w2.fs().read_all(&f).unwrap().expect("durable binding");
                (b.len() as u64, b)
            }) {
                reached_sizes.insert(len);
                for (i, u) in bytes.chunks(512).enumerate() {
                    if u.len() == 512 {
                        reached_units.insert((i, u.to_vec()));
                    }
                }
                assert!(
                    h.sizes.contains(&len),
                    "seed {seed} {mode:?}: size {len} not in {:?}",
                    h.sizes
                );
                for (i, u) in bytes.chunks(512).enumerate() {
                    assert!(h.unit_allowed(i, u), "seed {seed} {mode:?}: unit {i} not a version");
                }
            }
            // ...and every size and every full unit version of the reference is reachable.
            assert_eq!(reached_sizes, h.sizes, "seed {seed} {mode:?}: reachable sizes");
            let max = *h.sizes.iter().max().unwrap();
            for snap in &h.snapshots {
                for i in 0..(max / 512) as usize {
                    let mut u = vec![0u8; 512];
                    if snap.len() > i * 512 {
                        let end = snap.len().min((i + 1) * 512);
                        u[..end - i * 512].copy_from_slice(&snap[i * 512..end]);
                    }
                    assert!(
                        reached_units.contains(&(i, u)),
                        "seed {seed} {mode:?}: a version of unit {i} is unreachable"
                    );
                }
            }
        }
    }
    assert_eq!(images, 120);
}

#[test]
fn m4_extending_write_has_intermediate_sizes() {
    let w = World::new(cfg());
    let fs = w.fs();
    fs.create(&p("f")).unwrap();
    fs.sync_dir(Path::new("/dd")).unwrap();
    fs.write_all_at(&p("f"), 0, &[1u8; 1500]).unwrap();
    let img = w.capture();
    let sizes: BTreeSet<u64> =
        all_branches(&img, CrashKind::PowerLoss, cfg, |w2| w2.fs().len(&p("f")).unwrap().unwrap())
            .into_iter()
            .collect();
    assert_eq!(sizes, BTreeSet::from([0, 512, 1024, 1500]));
}

// ---------------------------------------------------------------------------------------------
// M5: directory operations survive independently; renames are atomic.

#[test]
fn m5_directory_operations() {
    let w = World::new(cfg());
    let fs = w.fs();
    fs.create(&p("a")).unwrap();
    fs.create(&p("b")).unwrap();
    let img = w.capture();
    let states: BTreeSet<(bool, bool)> = all_branches(&img, CrashKind::PowerLoss, cfg, |w2| {
        let fs = w2.fs();
        (fs.len(&p("a")).unwrap().is_some(), fs.len(&p("b")).unwrap().is_some())
    })
    .into_iter()
    .collect();
    // btrfs tree-log / ext4 fast_commit: `b` without the earlier `a`.
    assert!(states.contains(&(false, true)));
    assert_eq!(states.len(), 4);

    // A rename is atomic: never both names, never neither.
    let w = World::new(cfg());
    let fs = w.fs();
    fs.write_all_at(&p("a"), 0, b"x").unwrap();
    fs.sync_data(&p("a")).unwrap();
    fs.sync_dir(Path::new("/dd")).unwrap();
    fs.rename(&p("a"), &p("c")).unwrap();
    let img = w.capture();
    for (a, c) in all_branches(&img, CrashKind::PowerLoss, cfg, |w2| {
        let fs = w2.fs();
        (fs.len(&p("a")).unwrap().is_some(), fs.len(&p("c")).unwrap().is_some())
    }) {
        assert!(a != c, "rename not atomic: a={a} c={c}");
    }
    // ...unless A-REN is broken (W3).
    let w3 = || {
        cfg_with(Adversary::Witness(WitnessSet {
            break_rename: true,
            ..WitnessSet::default()
        }))
    };
    let broken: BTreeSet<(bool, bool)> = all_branches(&img, CrashKind::PowerLoss, w3, |w2| {
        let fs = w2.fs();
        (fs.len(&p("a")).unwrap().is_some(), fs.len(&p("c")).unwrap().is_some())
    })
    .into_iter()
    .collect();
    assert!(broken.contains(&(true, true)) || broken.contains(&(false, false)));

    // An unsynced unlink can resurrect the file.
    let w = World::new(cfg());
    let fs = w.fs();
    fs.write_all_at(&p("a"), 0, b"x").unwrap();
    fs.sync_data(&p("a")).unwrap();
    fs.sync_dir(Path::new("/dd")).unwrap();
    assert!(fs.remove(&p("a")).unwrap());
    let img = w.capture();
    let back: BTreeSet<bool> =
        all_branches(&img, CrashKind::PowerLoss, cfg, |w2| w2.fs().len(&p("a")).unwrap().is_some())
            .into_iter()
            .collect();
    assert_eq!(back, BTreeSet::from([false, true]));
}

// ---------------------------------------------------------------------------------------------
// M6: `kv_model` with a subset power-loss hook.

#[test]
fn m6_kv_model_with_subset_power_loss() {
    // Cells stay undecided until read, possibly epochs later, so every crash's decider carries
    // the choices of the earlier ones (rule L4).
    let choices = std::sync::Mutex::new(BTreeMap::new());
    let hook = |e: &dashd_store::faulty::FaultyEngine, keep: &[bool]| {
        let ids = e.pending_ids();
        assert_eq!(ids.len(), keep.len(), "the model and the engine agree on pending batches");
        let epoch = e.world().epoch() + 1;
        let mut all = choices.lock().unwrap();
        for (id, k) in ids.iter().zip(keep) {
            all.insert(format!("{epoch}:{}:b{id}", e.name()), u32::from(!*k));
        }
        e.world().crash_in_place(CrashKind::PowerLoss, Box::new(Fixed::new(all.clone())));
    };
    let mut losses = 0;
    let mut subsets = 0;
    let mut ranges = 0;
    for seed in 0..200 {
        choices.lock().unwrap().clear();
        let w = World::new(cfg());
        let e = (*w.engine("kv-model", &MODEL_SCHEMA)).clone();
        let ops = decode_ops(&pseudo_random_bytes(seed, 4096));
        for op in &ops {
            match op {
                dashd_store::model::Op::PowerLoss(n) => {
                    losses += 1;
                    // A mask that drops a batch and keeps a later one.
                    subsets += usize::from((0..7).any(|i| (n >> i) & 3 == 2));
                }
                dashd_store::model::Op::DeleteRange(_, start, end) => {
                    ranges += usize::from(start < end);
                }
                _ => {}
            }
        }
        let hooks = Hooks {
            power_loss_subset: Some(&hook),
            ..Hooks::default()
        };
        check_with(e, &ops, hooks).unwrap_or_else(|e| panic!("seed {seed}: {e}"));
    }
    assert!(losses > 100 && subsets > 50, "losses {losses}, non-prefix subsets {subsets}");
    assert!(ranges > 100, "range deletes {ranges}");
}

// ---------------------------------------------------------------------------------------------
// M6b: a range delete is one more operation of its batch (CH2a's `delete_range`). It deletes
// every key in `[start, end)` that no later put or delete of the batch names, is kept or lost
// with the batch, and D1's durable view sees it as reads do.

#[test]
fn m6b_range_deletes_belong_to_their_batch() {
    let w = World::new(cfg());
    let e = w.engine("m", &SCHEMA);
    let base: &[(Cf, &[u8], &[u8])] =
        &[(A, b"a", b"0"), (A, b"b", b"0"), (A, b"c", b"0"), (B, b"b", b"0")];
    put(&*e, base, Commit::Durable);
    let mut b = e.batch();
    b.put(A, b"a", b"1");
    b.delete_range(A, b"a", b"c");
    b.put(A, b"b", b"2");
    e.write(b, Commit::Buffered).unwrap();
    fn read(e: &impl KvRead) -> [Option<Vec<u8>>; 4] {
        [(A, &b"a"[..]), (A, b"b"), (A, b"c"), (B, b"b")].map(|(cf, k)| e.get(cf, k).unwrap())
    }
    let v = |s: &[u8]| Some(s.to_vec());
    let kept = [None, v(b"2"), v(b"0"), v(b"0")];
    let lost = [v(b"0"), v(b"0"), v(b"0"), v(b"0")];
    assert_eq!(read(&*e), kept);
    let img = w.capture();
    let states: BTreeSet<_> =
        all_branches(&img, CrashKind::PowerLoss, cfg, |w2| read(&*w2.engine("m", &SCHEMA)))
            .into_iter()
            .collect();
    assert_eq!(states, BTreeSet::from([kept.clone(), lost]));

    // Before any read decides the batch, D1's view knows only the keys it does not cover.
    let w2 = img.boot(CrashKind::PowerLoss, Box::new(AllSurvive), cfg());
    let view = w2.durable_view();
    assert_eq!(view.get("m", A, b"a"), Peek::Unknown);
    assert_eq!(view.get("m", A, b"b"), Peek::Unknown);
    assert_eq!(view.get("m", A, b"c"), Peek::Known(v(b"0")));
    assert_eq!(view.get("m", B, b"b"), Peek::Known(v(b"0")));
    // Once kept, the view and the records see the range as reads do.
    assert_eq!(read(&*w2.engine("m", &SCHEMA)), kept);
    let view = w2.durable_view();
    assert_eq!(view.get("m", A, b"a"), Peek::Known(None));
    assert_eq!(view.get("m", A, b"b"), Peek::Known(v(b"2")));
    let records: Vec<_> = view.records("m").into_iter().filter(|r| r.0 != Cf::new(0)).collect();
    assert_eq!(
        records,
        [
            (A, b"b".to_vec(), b"2".to_vec(), true),
            (A, b"c".to_vec(), b"0".to_vec(), true),
            (B, b"b".to_vec(), b"0".to_vec(), true),
        ]
    );
}

// ---------------------------------------------------------------------------------------------
// M7: fsyncgate.

#[test]
fn m7_failed_sync_poisons() {
    let fault = Fault {
        at: EventSel::Nth {
            store: StoreSel::Fs,
            op: OpKind::SyncData,
            n: 1,
        },
        kind: FaultKind::Io,
        outcome: FaultOutcome::Applied,
    };
    let mk = || WorldConfig {
        faults: vec![fault.clone()],
        ..cfg()
    };
    let w = World::new(mk());
    let fs = w.fs();
    fs.write_all_at(&p("f"), 0, &[1u8; 512]).unwrap();
    fs.sync_data(&p("f")).unwrap();
    fs.sync_dir(Path::new("/dd")).unwrap();
    fs.write_all_at(&p("f"), 0, &[2u8; 512]).unwrap();
    assert!(fs.sync_data(&p("f")).is_err(), "the injected failure");
    // A later sync reports success and syncs nothing.
    fs.sync_data(&p("f")).unwrap();
    assert!(
        w.durable_view().range_durable(&dashd_store::faulty::FileRef::new(p("f"), 0, 512)).is_err()
    );
    for kind in [CrashKind::PowerLoss, CrashKind::ProcessKill] {
        let img = w.capture();
        let mut seen = BTreeSet::new();
        for b in all_branches(&img, kind, cfg, |w2| {
            if kind == CrashKind::ProcessKill {
                // A process kill keeps the page cache; a later power loss still loses it.
                let img2 = w2.capture();
                all_branches(&img2, CrashKind::PowerLoss, cfg, |w3| {
                    w3.fs().read_all(&p("f")).unwrap().unwrap()[0]
                })
            } else {
                vec![w2.fs().read_all(&p("f")).unwrap().unwrap()[0]]
            }
        }) {
            seen.extend(b);
        }
        assert_eq!(seen, BTreeSet::from([1, 2]), "{kind:?}");
    }
}

// ---------------------------------------------------------------------------------------------
// M8: capacity, partial writes, advisory allocation, shared charging.

#[test]
fn m8_capacity_and_partial_writes() {
    let mk = |allocate| WorldConfig {
        capacity: Some(4096),
        allocate,
        ..cfg()
    };
    let w = World::new(mk(AllocateMode::Fallocate));
    let fs = w.fs();
    fs.write_all_at(&p("f"), 0, &[1u8; 3000]).unwrap();
    let err = fs.write_all_at(&p("f"), 3000, &[1u8; 2000]).unwrap_err();
    assert_eq!(err.raw_os_error(), Some(libc::ENOSPC));
    assert_eq!(fs.len(&p("f")).unwrap(), Some(3000));
    // Allocation past capacity is advisory: no error, no growth.
    fs.allocate(&p("f"), 3000, 5000);
    assert_eq!(fs.len(&p("f")).unwrap(), Some(3000));
    fs.allocate(&p("f"), 3000, 1000);
    assert_eq!(fs.len(&p("f")).unwrap(), Some(4000));
    assert_eq!(fs.available_space(Path::new("/dd")).unwrap(), 96);

    let w = World::new(mk(AllocateMode::ZeroFill));
    let fs = w.fs();
    fs.write_all_at(&p("f"), 0, &[9u8; 100]).unwrap();
    fs.allocate(&p("f"), 100, 1000);
    let all = fs.read_all(&p("f")).unwrap().unwrap();
    assert_eq!(all.len(), 1100);
    assert!(all[100..].iter().all(|&b| b == 0));

    // A partial write applies a prefix of granules.
    let partial = Fault {
        at: EventSel::Nth {
            store: StoreSel::Fs,
            op: OpKind::WriteAt,
            n: 0,
        },
        kind: FaultKind::NoSpace,
        outcome: FaultOutcome::Partial(1),
    };
    let w = World::new(WorldConfig {
        faults: vec![partial],
        ..cfg()
    });
    let fs = w.fs();
    assert!(fs.write_all_at(&p("f"), 0, &[5u8; 1500]).is_err());
    assert_eq!(fs.read_all(&p("f")).unwrap().unwrap(), vec![5u8; 512]);

    // Shared capacity: engine bytes count against the filesystem.
    let w = World::new(WorldConfig {
        capacity: Some(2000),
        shared_capacity: true,
        ..cfg()
    });
    let e = w.engine("m", &SCHEMA);
    put(&*e, &[(A, b"k", &[0u8; 1500])], Commit::Buffered);
    let fs = w.fs();
    assert!(fs.available_space(Path::new("/dd")).unwrap() < 600);
    assert!(fs.write_all_at(&p("f"), 0, &[1u8; 600]).is_err());
}

// ---------------------------------------------------------------------------------------------
// M9: link counts, external links, and a copy is an unsynced new file.

#[test]
fn m9_links_and_copies() {
    let w = World::new(cfg());
    let fs = w.fs();
    fs.write_all_at(&p("f"), 0, b"data").unwrap();
    fs.sync_data(&p("f")).unwrap();
    fs.sync_dir(Path::new("/dd")).unwrap();
    assert_eq!(fs.link_count(&p("f")).unwrap(), Some(1));
    fs.link_external(&p("f"), Path::new("/oracle/blk"));
    assert_eq!(fs.link_count(&p("f")).unwrap(), Some(2));
    assert_eq!(fs.link_count(&p("g")).unwrap(), None);
    fs.copy(&p("f"), &p("g")).unwrap();
    assert_eq!(fs.read_all(&p("g")).unwrap().unwrap(), b"data");
    assert_eq!(fs.link_count(&p("g")).unwrap(), Some(1));
    let img = w.capture();
    let g: BTreeSet<Option<Vec<u8>>> =
        all_branches(&img, CrashKind::PowerLoss, cfg, |w2| w2.fs().read_all(&p("g")).unwrap())
            .into_iter()
            .collect();
    assert!(g.contains(&None), "the copy's entry is not durable");
    assert!(g.iter().any(|c| c.as_ref().is_some_and(|c| c.is_empty())), "nor its data");
    for b in all_branches(&img, CrashKind::PowerLoss, cfg, |w2| {
        w2.fs().read_external(Path::new("/oracle/blk"))
    }) {
        assert_eq!(b, Some(b"data".to_vec()));
    }
}

// ---------------------------------------------------------------------------------------------
// M11: determinism.

fn workload(w: &World) {
    let e = w.engine("m", &SCHEMA);
    let fs = w.fs();
    let mut rng = Seeded::new(42);
    for i in 0..50u8 {
        if rng.chance(500) {
            put(
                &*e,
                &[(A, &[i % 5], &[i])],
                if rng.chance(200) {
                    Commit::Durable
                } else {
                    Commit::Buffered
                },
            );
        } else {
            fs.write_all_at(&p("f"), rng.below(2000), &[i; 300]).unwrap();
            if rng.chance(200) {
                fs.sync_data(&p("f")).unwrap();
            }
        }
        let _ = e.get(A, &[i % 5]).unwrap();
    }
}

#[test]
fn m11_runs_are_deterministic() {
    let hashes: Vec<(u64, Vec<_>)> = (0..2)
        .map(|_| {
            let w = World::new(WorldConfig {
                record_observations: true,
                ..cfg()
            });
            workload(&w);
            let img = w.capture();
            let w2 = img.boot(
                CrashKind::PowerLoss,
                Box::new(SeededDecider::new(5)),
                WorldConfig {
                    record_observations: true,
                    ..cfg()
                },
            );
            let _ = w2.fs().read_all(&p("f")).unwrap();
            for k in 0..5u8 {
                let _ = w2.engine("m", &SCHEMA).get(A, &[k]).unwrap();
            }
            (w2.trace().hash(), w2.observations())
        })
        .collect();
    assert_eq!(hashes[0], hashes[1]);
}

// ---------------------------------------------------------------------------------------------
// M15: after `freeze`, no call changes any image.

#[test]
fn m15_freeze() {
    let w = World::new(cfg());
    workload(&w);
    let before = w.capture();
    w.freeze();
    let e = w.engine("m", &SCHEMA);
    let mut b = e.batch();
    b.put(A, b"z", b"z");
    assert!(e.write(b, Commit::Durable).is_err());
    assert!(w.fs().write_all_at(&p("f"), 0, b"zz").is_err());
    assert!(e.sync_wal().is_err());
    let after = w.capture();
    let obs = |img: &CrashImage| {
        all_branches(img, CrashKind::PowerLoss, cfg, |w2| {
            (w2.fs().read_all(&p("f")).unwrap(), w2.engine("m", &SCHEMA).get(A, b"z").unwrap())
        })
    };
    assert_eq!(obs(&before), obs(&after));
}

// ---------------------------------------------------------------------------------------------
// M16: RocksDB's observed non-prefix state, and the finalizer states, are in the subset domain.

#[test]
fn m16_native_states_are_in_the_domain() {
    let w = World::new(cfg());
    let e = w.engine("m", &SCHEMA);
    put(&*e, &[(A, b"b1", b"v")], Commit::Durable);
    for k in [b"b2", b"b3", b"b4"] {
        put(&*e, &[(A, k, b"v")], Commit::Buffered);
    }
    let img = w.capture();
    let states: BTreeSet<Vec<Vec<u8>>> = all_branches(&img, CrashKind::PowerLoss, cfg, |w2| {
        w2.engine("m", &SCHEMA).iter_prefix(A, b"").map(|r| r.unwrap().0).collect()
    })
    .into_iter()
    .collect();
    assert!(states.contains(&vec![b"b1".to_vec(), b"b3".to_vec(), b"b4".to_vec()]));
    assert_eq!(states.len(), 8);

    // §6.4 case 3: a finalizer whose Durable append survived while the barrier did not return,
    // and the earlier Buffered batch it presumes did not.
    let crash_before_barrier = |barrier_first: bool| {
        let w = World::new(WorldConfig {
            faults: vec![Fault {
                at: EventSel::Nth {
                    store: StoreSel::Any,
                    op: OpKind::Barrier,
                    n: 0,
                },
                kind: FaultKind::Io,
                outcome: FaultOutcome::NotApplied,
            }],
            ..cfg()
        });
        let e = w.engine("m", &SCHEMA);
        put(&*e, &[(A, b"bj", b"coin")], Commit::Buffered);
        if barrier_first {
            e.sync_wal().unwrap();
        }
        let mut b = e.batch();
        b.put(A, b"bk", b"tip");
        assert!(e.write(b, Commit::Durable).is_err(), "the barrier did not complete");
        let img = w.capture();
        all_branches(&img, CrashKind::PowerLoss, cfg, |w2| {
            let e = w2.engine("m", &SCHEMA);
            (e.get(A, b"bj").unwrap().is_some(), e.get(A, b"bk").unwrap().is_some())
        })
        .into_iter()
        .collect::<BTreeSet<_>>()
    };
    assert!(crash_before_barrier(false).contains(&(false, true)), "case 3: bk without bj");
    assert!(crash_before_barrier(true).iter().all(|(bj, _)| *bj), "case 4: bj always kept");
}

// ---------------------------------------------------------------------------------------------
// M17: a batch is atomic across column families, except under W2.

#[test]
fn m17_batches_are_atomic_across_cfs() {
    let w = World::new(cfg());
    let e = w.engine("m", &SCHEMA);
    put(&*e, &[(A, b"k", b"1"), (B, b"k", b"1")], Commit::Buffered);
    let img = w.capture();
    let read = |w2: &World| {
        let e = w2.engine("m", &SCHEMA);
        (e.get(A, b"k").unwrap().is_some(), e.get(B, b"k").unwrap().is_some())
    };
    let atomic: BTreeSet<_> =
        all_branches(&img, CrashKind::PowerLoss, cfg, read).into_iter().collect();
    assert_eq!(atomic, BTreeSet::from([(false, false), (true, true)]));
    let w2 = || {
        cfg_with(Adversary::Witness(WitnessSet {
            break_atomic: true,
            ..WitnessSet::default()
        }))
    };
    let split: BTreeSet<_> =
        all_branches(&img, CrashKind::PowerLoss, w2, read).into_iter().collect();
    assert_eq!(split.len(), 4);
}

// ---------------------------------------------------------------------------------------------
// M18: rule L4 across nested crashes, against an independent reference: the outer batch's
// fate is fixed by the first crash (it is durable after it, whatever it is), and the inner
// batch's depends only on the inner crash kind.

#[test]
fn m18_nested_cells_keep_their_provenance() {
    let w = World::new(cfg());
    put(&*w.engine("m", &SCHEMA), &[(A, b"k", b"1")], Commit::Buffered);
    let img = w.capture();
    for inner in [CrashKind::PowerLoss, CrashKind::ProcessKill] {
        for read_before in [false, true] {
            // Every branch: (k seen in epoch 1 if read there, k in epoch 2, x in epoch 2).
            let states: BTreeSet<(Option<bool>, bool, bool)> =
                all_branches(&img, CrashKind::PowerLoss, cfg, |w1| {
                    let e1 = w1.engine("m", &SCHEMA);
                    let k1 = read_before.then(|| e1.get(A, b"k").unwrap().is_some());
                    put(&*e1, &[(B, b"x", b"y")], Commit::Buffered);
                    let img1 = w1.capture();
                    // The inner crash's branches, with the outer decisions carried (L4).
                    all_branches(&img1, inner, cfg, |w2| {
                        let e2 = w2.engine("m", &SCHEMA);
                        (k1, e2.get(A, b"k").unwrap().is_some(), e2.get(B, b"x").unwrap().is_some())
                    })
                })
                .into_iter()
                .flatten()
                .collect();
            // The reference.
            let mut want = BTreeSet::new();
            for k in [false, true] {
                let xs: &[bool] = match inner {
                    CrashKind::PowerLoss => &[false, true],
                    CrashKind::ProcessKill => &[true],
                };
                for &x in xs {
                    want.insert((read_before.then_some(k), k, x));
                }
            }
            assert_eq!(states, want, "inner {inner:?}, read before the inner crash: {read_before}");
        }
    }
}

#[test]
fn decisions_respect_domains() {
    struct Wild;
    impl Decider for Wild {
        fn choose(&mut self, v: &dashd_store::faulty::Var) -> u32 {
            v.domain - 1
        }
    }
    let w = World::new(WorldConfig {
        granularity: Granularity::Page,
        ..cfg()
    });
    w.fs().write_all_at(&p("f"), 0, &[1u8; 9000]).unwrap();
    let img = w.capture();
    let w2 = img.boot(CrashKind::PowerLoss, Box::new(Wild), cfg());
    assert_eq!(w2.fs().len(&p("f")).unwrap(), None, "the entry was lost");
}

// ---------------------------------------------------------------------------------------------
// M10: lazy = eager on observation traces.

/// A post-crash program over every `FlatFs` call and the engine's appends, barriers and reads,
/// with a nested power loss in the middle. Returns what each call returned.
fn program(w0: World, seed: u64, dfs: &Dfs, eager: bool) -> Vec<String> {
    let mut rng = Seeded::new(seed ^ 0xabcd);
    let mut out = Vec::new();
    let mut w = w0;
    if eager {
        w.force_all();
    }
    let crash_at = 3 + rng.below(4);
    let files = [p("a"), p("b"), p("d/c")];
    for step in 0..9 {
        if step == crash_at {
            let img = w.capture();
            w = img.boot(CrashKind::PowerLoss, Box::new(dfs.clone()), cfg());
            if eager {
                w.force_all();
            }
            out.push("crash".into());
        }
        let fs = w.fs();
        let e = w.engine("m", &SCHEMA);
        let f = &files[rng.below(3) as usize];
        let g = &files[rng.below(3) as usize];
        let r = match rng.below(17) {
            0 => format!("{:?}", fs.read_all(f).map_err(|e| e.kind())),
            1 => format!("{:?}", fs.len(f).map_err(|e| e.kind())),
            2 => format!("{:?}", fs.link_count(f).map_err(|e| e.kind())),
            3 => format!("{:?}", e.get(A, &[rng.below(3) as u8]).map_err(|e| e.to_string())),
            4 => format!("{:?}", e.iter_prefix(A, b"").map(|r| r.unwrap()).collect::<Vec<_>>()),
            5 => format!(
                "{:?}",
                fs.write_all_at(f, rng.below(1200), &[step as u8 + 1; 120]).map_err(|e| e.kind())
            ),
            6 => format!("{:?}", fs.truncate(f, rng.below(1500)).map_err(|e| e.kind())),
            7 => {
                fs.allocate(f, rng.below(1000), 700);
                "alloc".into()
            }
            8 => format!("{:?}", fs.sync_data(f).map_err(|e| e.kind())),
            9 => format!("{:?}", fs.sync_dir(Path::new("/dd")).map_err(|e| e.kind())),
            10 => format!("{:?}", fs.copy(f, g).map_err(|e| e.kind())),
            11 if f.parent() == g.parent() && f != g => {
                format!("{:?}", fs.rename(f, g).map_err(|e| e.kind()))
            }
            12 => format!("{:?}", fs.remove(f).map_err(|e| e.kind())),
            13 => format!("{:?}", fs.create(f).map_err(|e| e.kind())),
            14 => format!("{:?}", fs.create_dir_all(&p("d")).map_err(|e| e.kind())),
            15 => {
                let mut b = e.batch();
                b.put(A, &[rng.below(3) as u8], &[step as u8]);
                format!(
                    "{:?}",
                    e.write(
                        b,
                        if rng.chance(300) {
                            Commit::Durable
                        } else {
                            Commit::Buffered
                        }
                    )
                    .is_ok()
                )
            }
            _ => format!("{:?}", e.sync_wal().is_ok()),
        };
        out.push(r);
    }
    // Finally observe everything.
    let fs = w.fs();
    for f in &files {
        out.push(format!("{:?}", fs.read_all(f).map_err(|e| e.kind())));
    }
    let e = w.engine("m", &SCHEMA);
    out.push(format!("{:?}", e.iter_prefix(A, b"").map(|r| r.unwrap()).collect::<Vec<_>>()));
    out
}

/// Compares lazy and eager observation traces over `seeds` random images; seeds whose eager
/// product exceeds `cap` branches are skipped (at most `max_skipped`).
fn m10(seeds: u64, cap: u32, max_skipped: u32) {
    let mut compared = 0;
    let mut skipped = 0;
    for seed in 0..seeds {
        let w = World::new(cfg());
        let e = w.engine("m", &SCHEMA);
        let fs = w.fs();
        let mut rng = Seeded::new(seed);
        fs.create_dir_all(&p("d")).unwrap();
        for i in 0..2 + rng.below(3) {
            match rng.below(4) {
                0 => put(&*e, &[(A, &[rng.below(3) as u8], &[i as u8])], Commit::Buffered),
                1 => fs
                    .write_all_at(
                        &p(["a", "b"][rng.below(2) as usize]),
                        rng.below(1000),
                        &[i as u8 + 1; 200],
                    )
                    .unwrap(),
                2 => fs.sync_data(&p("a")).map(drop).unwrap_or(()),
                _ => fs.truncate(&p("b"), rng.below(900)).unwrap(),
            }
        }
        let img = w.capture();
        // `None` if the eager product is too large to enumerate here.
        let traces = |eager: bool| -> Option<BTreeSet<Vec<String>>> {
            let dfs = Dfs::new();
            let mut set = BTreeSet::new();
            let mut n = 0;
            loop {
                let w = img.boot(CrashKind::PowerLoss, dfs.decider(), cfg());
                set.insert(program(w, seed, &dfs, eager));
                n += 1;
                if n > cap {
                    return None;
                }
                if !dfs.advance() {
                    return Some(set);
                }
            }
        };
        let Some(eager) = traces(true) else {
            skipped += 1;
            continue;
        };
        let lazy = traces(false).expect("lazy never has more branches than eager");
        assert_eq!(lazy, eager, "seed {seed}");
        compared += lazy.len();
    }
    assert!(skipped <= max_skipped, "{skipped} seeds too large to enumerate eagerly");
    assert!(compared as u64 > seeds * 4, "{compared} distinct traces compared");
}

#[test]
fn m10_lazy_equals_eager() {
    m10(40, 1500, 20);
}

/// M10 at scale (nightly, §8.4).
#[test]
#[ignore]
fn m10_lazy_equals_eager_at_scale() {
    m10(400, 20_000, 60);
}

// ---------------------------------------------------------------------------------------------
// M12: coverage and closed-form work counts.

/// A trivial system: each op commits one Buffered batch with its own key; opening reads them
/// all.
struct Counter {
    e: std::sync::Arc<dashd_store::faulty::FaultyEngine>,
}

#[derive(Debug, Clone)]
struct Unit;

impl std::fmt::Display for Unit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("u")
    }
}

impl std::str::FromStr for Unit {
    type Err = ();
    fn from_str(_: &str) -> Result<Self, ()> {
        Ok(Self)
    }
}

struct NoRef;

impl dashd_store::faulty::Reference for NoRef {
    type Op = u8;
    type Config = Unit;
    fn new(_: &Unit) -> Self {
        Self
    }
    fn apply(&mut self, _: &u8) {}
}

impl dashd_store::faulty::Sut for Counter {
    type Config = Unit;
    type Op = u8;
    type Ref = NoRef;
    const NAME: &'static str = "counter";

    fn open(w: &World, _: &Unit) -> Result<Self, dashd_store::faulty::Abort> {
        let e = w.engine("m", &SCHEMA);
        for k in 0..16u8 {
            let _ = e.get(A, &[k]).unwrap();
        }
        Ok(Self {
            e,
        })
    }

    fn apply(&mut self, op: &u8) -> Result<(), dashd_store::faulty::Abort> {
        put(&*self.e, &[(A, &[*op], b"v")], Commit::Buffered);
        Ok(())
    }

    fn check(
        &self,
        _: &World,
        _: &NoRef,
        _: &dashd_store::faulty::Trace,
    ) -> Result<(), Vec<dashd_store::faulty::Violation>> {
        Ok(())
    }
}

#[test]
fn m12_coverage_and_work_counts() {
    use dashd_store::faulty::{Mode, Scenario, explore};
    for n in 1..=6u8 {
        let sc = Scenario::<Counter>::new("counter", Unit, (0..n).collect());
        let r = explore(
            &sc,
            Mode::Exhaustive {
                max_branches_per_image: 1024,
            },
        );
        assert!(r.is_clean(), "{}", r.summary());
        // Every event once: one image per batch.
        assert_eq!(r.images, u64::from(n));
        // After batch j: one process-kill branch, and 2^j power-loss branches.
        let want: u64 = (1..=u64::from(n)).map(|j| 1 + (1 << j)).sum();
        assert_eq!(r.branches, want);
        assert_eq!(r.max_branches_per_image, 1 << n);
        // One event per batch; recovery only reads.
        assert_eq!(r.work.events, u64::from(n));
        // One decision per pending batch read on every power-loss branch.
        let decisions: u64 = (1..=u64::from(n)).map(|j| j * (1 << j)).sum();
        assert_eq!(r.work.decisions, decisions);
    }
}

// ---------------------------------------------------------------------------------------------
// M13: the minimiser reaches a fixed point; 1-minimality is verified or not claimed.

#[test]
fn m13_minimiser_fixed_point() {
    use dashd_store::faulty::minimize_decisions;
    // GPT's non-monotone case: fails(a, b) = a ∨ ¬b. One pass toward 0 leaves (1, 0); the fixed
    // point reaches (0, 0).
    let start = BTreeMap::from([("a".to_string(), 1), ("b".to_string(), 1)]);
    let fails = |d: &BTreeMap<String, u32>| d.get("a") == Some(&1) || !d.contains_key("b");
    let (min, verified) = minimize_decisions(&start, fails);
    assert!(min.is_empty(), "{min:?}");
    assert!(verified);
    // Two decisions that must both stay: verified 1-minimal.
    let start = BTreeMap::from([("x".to_string(), 2), ("y".to_string(), 1), ("z".to_string(), 1)]);
    let both = |d: &BTreeMap<String, u32>| d.contains_key("x") && d.contains_key("y");
    let (min, verified) = minimize_decisions(&start, both);
    assert_eq!(min, BTreeMap::from([("x".to_string(), 1), ("y".to_string(), 1)]));
    assert!(verified);
}

// ---------------------------------------------------------------------------------------------
// Regressions from the checkpoint-1 review.

/// A process kill can only tear the write it interrupts, and never regrows a synced size.
#[test]
fn process_kill_tears_only_the_last_write() {
    let w = World::new(cfg());
    let fs = w.fs();
    let e = w.engine("m", &SCHEMA);
    fs.create(&p("f")).unwrap();
    fs.sync_dir(Path::new("/dd")).unwrap();
    fs.write_all_at(&p("f"), 0, &[1u8; 12288]).unwrap();
    let torn = w.capture();
    put(&*e, &[(A, b"k", b"v")], Commit::Buffered);
    let later = w.capture();
    let sizes = |img: &CrashImage| -> BTreeSet<u64> {
        all_branches(img, CrashKind::ProcessKill, cfg, |w2| w2.fs().len(&p("f")).unwrap().unwrap())
            .into_iter()
            .collect()
    };
    assert_eq!(sizes(&torn), BTreeSet::from([4096, 8192, 12288]));
    assert_eq!(sizes(&later), BTreeSet::from([12288]));
    fs.sync_data(&p("f")).unwrap();
    fs.truncate(&p("f"), 0).unwrap();
    fs.sync_data(&p("f")).unwrap();
    assert_eq!(sizes(&w.capture()), BTreeSet::from([0]));
}

/// fsyncgate poisons the writes that failed to sync, not the file: later writes sync.
#[test]
fn poisoning_is_per_write() {
    let fault = Fault {
        at: EventSel::Nth {
            store: StoreSel::Fs,
            op: OpKind::SyncData,
            n: 0,
        },
        kind: FaultKind::Io,
        outcome: FaultOutcome::Applied,
    };
    let w = World::new(WorldConfig {
        faults: vec![fault],
        ..cfg()
    });
    let fs = w.fs();
    fs.create(&p("f")).unwrap();
    fs.sync_dir(Path::new("/dd")).unwrap();
    fs.write_all_at(&p("f"), 0, &[1u8; 512]).unwrap();
    assert!(fs.sync_data(&p("f")).is_err());
    let img = w.capture();
    let w2 = img.boot(CrashKind::ProcessKill, Box::new(AllSurvive), cfg());
    let fs2 = w2.fs();
    fs2.write_all_at(&p("f"), 1024, &[3u8; 512]).unwrap();
    fs2.sync_data(&p("f")).unwrap();
    let after = w2.capture();
    let got: BTreeSet<(u8, u8)> = all_branches(&after, CrashKind::PowerLoss, cfg, |w3| {
        let b = w3.fs().read_all(&p("f")).unwrap().unwrap();
        (b[0], b[1024])
    })
    .into_iter()
    .collect();
    // The poisoned write may be lost; the later synced one is not.
    assert_eq!(got, BTreeSet::from([(0, 3), (1, 3)]));
}

/// A copy is a shrink to 0 and an extending write: short copies are reachable.
#[test]
fn copy_has_intermediate_sizes() {
    let w = World::new(cfg());
    let fs = w.fs();
    fs.write_all_at(&p("a"), 0, &[7u8; 1500]).unwrap();
    fs.sync_data(&p("a")).unwrap();
    fs.create(&p("b")).unwrap();
    fs.sync_dir(Path::new("/dd")).unwrap();
    fs.copy(&p("a"), &p("b")).unwrap();
    let img = w.capture();
    let sizes: BTreeSet<u64> =
        all_branches(&img, CrashKind::PowerLoss, cfg, |w2| w2.fs().len(&p("b")).unwrap().unwrap())
            .into_iter()
            .collect();
    assert_eq!(sizes, BTreeSet::from([0, 512, 1024, 1500]));
}

/// `allocate` zero-fills, even over data a crash kept past the reverted size.
#[test]
fn allocate_reads_zeros_over_stale_units() {
    let w = World::new(cfg());
    let fs = w.fs();
    fs.create(&p("f")).unwrap();
    fs.sync_dir(Path::new("/dd")).unwrap();
    fs.write_all_at(&p("f"), 0, &[9u8; 1024]).unwrap();
    let img = w.capture();
    // The size went back to 0 while the units survived.
    let d = Fixed::new(BTreeMap::from([("1:fs:i1:len".to_string(), 2)]));
    let w2 = img.boot(CrashKind::PowerLoss, Box::new(d), cfg());
    let fs2 = w2.fs();
    assert_eq!(fs2.len(&p("f")).unwrap(), Some(0));
    fs2.allocate(&p("f"), 0, 1024);
    assert_eq!(fs2.read_all(&p("f")).unwrap().unwrap(), vec![0u8; 1024]);
}

/// D1(a) compares bytes: a pending append sharing a synced record's last sector does not make
/// the record non-durable (A-PSOW).
#[test]
fn d1_shared_tail_sector_is_durable() {
    let w = World::new(cfg());
    let fs = w.fs();
    fs.create(&p("f")).unwrap();
    fs.sync_dir(Path::new("/dd")).unwrap();
    fs.write_all_at(&p("f"), 0, &[1u8; 300]).unwrap();
    fs.sync_data(&p("f")).unwrap();
    fs.write_all_at(&p("f"), 300, &[2u8; 300]).unwrap();
    let view = w.durable_view();
    assert!(view.range_durable(&dashd_store::faulty::FileRef::new(p("f"), 0, 300)).is_ok());
    assert!(view.range_durable(&dashd_store::faulty::FileRef::new(p("f"), 0, 301)).is_err());
}

/// Reads can fail by injection (§4.4), without being crash points.
#[test]
fn read_faults() {
    let fault = |op| Fault {
        at: EventSel::Nth {
            store: StoreSel::Any,
            op,
            n: 0,
        },
        kind: FaultKind::Io,
        outcome: FaultOutcome::NotApplied,
    };
    let w = World::new(WorldConfig {
        faults: vec![fault(OpKind::Read), fault(OpKind::Len)],
        ..cfg()
    });
    let fs = w.fs();
    fs.write_all_at(&p("f"), 0, b"x").unwrap();
    assert!(fs.len(&p("f")).is_err());
    assert_eq!(fs.len(&p("f")).unwrap(), Some(1));
    let mut b = [0u8];
    assert!(fs.read_exact_at(&p("f"), 0, &mut b).is_err(), "the first read of any store fails");
    assert_eq!(w.events(), 1, "reads are not events");
}

/// The stage decides, as for `OsFs` (CH2b's `OpenFailed`): a lost file, or one in a missing
/// directory, did not open; a short read of a present file and an injected read fault on it did
/// not fail to open, so they stay plain. Each keeps its kind.
#[test]
fn a_lost_file_did_not_open() {
    use std::io::ErrorKind;
    let w = World::new(WorldConfig {
        faults: vec![Fault {
            at: EventSel::Nth {
                store: StoreSel::Fs,
                op: OpKind::Read,
                n: 2,
            },
            kind: FaultKind::Io,
            outcome: FaultOutcome::NotApplied,
        }],
        ..cfg()
    });
    let fs = w.fs();
    fs.write_all_at(&p("f"), 0, b"x").unwrap();
    let mut b = [0u8; 2];
    let lost = fs.read_exact_at(&p("g"), 0, &mut b).unwrap_err();
    assert!(OpenFailed::is(&lost) && lost.kind() == ErrorKind::NotFound, "{lost:?}");
    let no_dir = fs.read_exact_at(Path::new("/nodir/f"), 0, &mut b).unwrap_err();
    assert!(OpenFailed::is(&no_dir) && no_dir.kind() == ErrorKind::NotFound, "{no_dir:?}");
    let injected = fs.read_exact_at(&p("f"), 0, &mut b[..1]).unwrap_err();
    assert!(!OpenFailed::is(&injected), "{injected:?}");
    assert_eq!(injected.raw_os_error(), Some(libc::EIO));
    let short = fs.read_exact_at(&p("f"), 0, &mut b).unwrap_err();
    assert!(!OpenFailed::is(&short) && short.kind() == ErrorKind::UnexpectedEof, "{short:?}");
    fs.read_exact_at(&p("f"), 0, &mut b[..1]).unwrap();
    assert_eq!(b[0], b'x');
}
