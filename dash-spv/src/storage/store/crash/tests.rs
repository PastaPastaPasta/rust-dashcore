use dash_chain::ChainLockRef;
use dash_kv::faulty::{
    explore, replay, sweep, CommitKind, CrashKind, Decider, EventSel, Fault, FaultKind,
    FaultOutcome, FaultPlan, Mode, Nested, OpKind, StoreSel, Var, WorldConfig,
};

use super::*;

const EXHAUSTIVE: Mode = Mode::Exhaustive {
    max_branches_per_image: 4096,
};

fn op(s: &str) -> Op {
    s.parse().expect("an op")
}

fn ops(s: &str) -> Vec<Op> {
    s.split_whitespace().map(op).collect()
}

fn scenario(bug: Option<Bug>) -> Scenario<StoreSut> {
    Scenario::new(
        "sb13",
        Config {
            bug,
            ..Config::default()
        },
        ops(WORKLOAD),
    )
}

/// The workload on the legacy header path (what dash-spv writes until SB14).
fn legacy_scenario() -> Scenario<StoreSut> {
    Scenario::new(
        "sb13-legacy",
        Config {
            headers: HeaderPath::Legacy,
            ..Config::default()
        },
        ops(LEGACY_WORKLOAD),
    )
}

#[test]
fn ops_and_configs_round_trip() {
    for op in ops(&format!("{WORKLOAD} r0+1:0 t4 k0 C")) {
        assert_eq!(op.to_string().parse(), Ok(op));
    }
    for bad in ["h0", "r1+0:1", "r1:1", "x3", "f", "t", ""] {
        assert_eq!(bad.parse::<Op>(), Err(()), "{bad}");
    }
    for cfg in ["engine=prefix bug=none headers=step", "engine=subset bug=W1 headers=legacy"] {
        assert_eq!(cfg.parse::<Config>().map(|c| c.to_string()).as_deref(), Ok(cfg));
    }
}

/// Core's locator heights (chain.cpp:32-49): eleven entries a step apart, then doubling steps,
/// then the anchor.
#[test]
fn locator_heights_are_cores() {
    assert_eq!(locator_heights(0), [0]);
    assert_eq!(locator_heights(3), [3, 2, 1, 0]);
    let mut want: Vec<u32> = (100..=110).rev().collect();
    want.extend([99, 97, 93, 85, 69, 37, 0]);
    assert_eq!(locator_heights(110), want);
}

#[test]
fn the_store_is_clean_on_prefix_engines() {
    let mut sc = scenario(None);
    sc.min_images = 20;
    let r = explore(&sc, EXHAUSTIVE);
    eprintln!("clean: {}", r.summary());
    assert!(r.is_clean(), "{}", r.summary());
    assert!(r.branches > r.images, "{}", r.summary());
}

/// The legacy header path, crashing between a reorg's truncation and its branch, and between
/// either and the subscribers' truncations: recovery rewinds each subscriber to the fork (D5).
#[test]
fn the_legacy_header_path_is_clean() {
    let mut sc = legacy_scenario();
    sc.min_images = 20;
    let r = explore(&sc, EXHAUSTIVE);
    eprintln!("legacy: {}", r.summary());
    assert!(r.is_clean(), "{}", r.summary());
    let mut sc = legacy_scenario();
    sc.nested = Nested::Depth1;
    let r = explore(&sc, EXHAUSTIVE);
    eprintln!("legacy nested: {}", r.summary());
    assert!(r.is_clean(), "{}", r.summary());
}

/// Crashes during recovery too (its rewinds, the anchor step and the wallet's E3 rewind are
/// Durable writes): every interrupted recovery converges (R7a).
#[test]
fn the_store_is_clean_with_nested_crashes() {
    let mut sc = scenario(None);
    sc.nested = Nested::Depth1;
    let r = explore(&sc, EXHAUSTIVE);
    eprintln!("nested: {}", r.summary());
    assert!(r.is_clean(), "{}", r.summary());
    assert!(r.r7a.converged > 0, "{}", r.summary());
}

/// A failing call in each database alone (the faulty wallet store, then the store), applied or
/// not before it fails: the system stops, and every crash image after it recovers.
#[test]
fn the_store_is_clean_with_a_failing_call_in_either_database() {
    let fault = |store: &str, op, n, outcome| Fault {
        at: EventSel::Nth {
            store: StoreSel::Engine(store.into()),
            op,
            n,
        },
        kind: FaultKind::Io,
        outcome,
    };
    let buffered = OpKind::Append(CommitKind::Buffered);
    let durable = OpKind::Append(CommitKind::Durable);
    let mut sc = scenario(None);
    sc.faults = FaultPlan(vec![
        fault("wallet", buffered, 1, FaultOutcome::NotApplied),
        fault("wallet", buffered, 2, FaultOutcome::Applied),
        fault("wallet", OpKind::SyncWal, 1, FaultOutcome::NotApplied),
        fault("spv", buffered, 4, FaultOutcome::NotApplied),
        fault("spv", buffered, 6, FaultOutcome::Applied),
        fault("spv", durable, 3, FaultOutcome::Applied),
    ]);
    let r = explore(&sc, EXHAUSTIVE);
    eprintln!("faults: {}", r.summary());
    assert!(r.is_clean(), "{}", r.summary());
}

/// Each planted bug is found with the invariant expected, and its repro replays.
fn witness(bug: Bug, expect: InvariantId) {
    let r = explore(
        &scenario(Some(bug)),
        Mode::Witness {
            expect: vec![expect],
            max_branches: 50_000,
        },
    );
    assert!(r.found_all, "{bug:?}: {expect:?} not found: {}", r.summary());
    let f = r.first(expect).expect("found");
    let got = replay::<StoreSut>(&f.repro).expect("replays");
    assert!(got.iter().any(|v| v.invariant == expect), "{bug:?}: the repro gives {got:?}");
}

/// W1 needs a power loss that keeps the wallet's step and loses the store's header step it
/// names (each database crashing on its own); W2 a boundary the wallet does not follow.
#[test]
fn planted_wallet_bugs_are_found() {
    witness(Bug::W1, InvariantId::S2);
    witness(Bug::W2, InvariantId::S3);
}

/// The seeded sweep: random workloads with crash points, kinds, injected faults in either
/// database and nested crashes.
#[test]
fn sweep_is_clean() {
    let r = sweep(500, 0x5b13_c0de, &random_scenario);
    eprintln!("sweep: {}", r.summary());
    assert!(r.is_clean(), "{}", r.summary());
    assert!(r.images > 400, "{}", r.summary());
}

/// A power loss that drops every pending commit of the store and keeps the wallet's: each
/// database crashes on its own.
struct LoseStorePending;

impl Decider for LoseStorePending {
    fn choose(&mut self, var: &Var) -> u32 {
        if var.name.contains(":spv:") {
            var.domain - 1
        } else {
            0
        }
    }
}

/// DEC-811: the drop of SB13 D6's message log is Durable: a power loss that loses every
/// pending commit of the store right after it brings no row back.
#[test]
fn the_message_log_drop_survives_a_power_loss() {
    let world = World::new(WorldConfig::default());
    let cfg = Config::default();
    let store = open_store(&world, &cfg).unwrap();
    store.put_mn_log_row(5, 0, b"diff").unwrap();
    store.sync_wal().unwrap();
    assert!(store.drop_mn_log().unwrap());
    drop(store);
    world.crash_in_place(CrashKind::PowerLoss, Box::new(LoseStorePending));
    let store = open_store(&world, &cfg).unwrap();
    assert_eq!(store.mn_log_rows().unwrap(), 0);
}

/// R6-M2's witness, the store's half (DEC-115; SB14 owns the wallet's rescan): A1..A100
/// durable with the ChainLock floor at A70, a Buffered reorg at 80 published as B110, the
/// wallet durably at B110, then a power loss. The store's durable floor and tip are A70 and
/// A80 (SB14: the reorg's lowering is Durable, its activation Buffered), and no B record
/// survives; the wallet, whose B110 the store lost, rewinds by its
/// locator (E3) to A69, its first entry on the retained chain.
#[test]
fn r6_m2_store_witness() {
    let world = World::new(WorldConfig::default());
    let cfg = Config::default();
    let (_, wallet) = engines(&world, &cfg);
    let sys = System::open(&world, wallet, &cfg).unwrap();
    sys.header_step(u32::MAX, 100, 0).unwrap();
    let a = sys.chain().unwrap();
    let id = |s: &System, x: &Hash32| s.store.by_hash(&BlockHash::from(*x)).unwrap().map(|r| r.id);
    let a70 = id(&sys, &a[70]).unwrap();
    // The floor at A70, with the step that makes A1..A100 durable.
    let meta = sys.store.view().meta.unwrap();
    let floor = ClPersisted {
        best: Some(ChainLockRef {
            height: 70,
            hash: BlockHash::from(a[70]),
        }),
        floor: Some(a70),
        enabled_observed: true,
    };
    let step = sys.store.begin_step().unwrap();
    let tail = StepTail {
        active: None,
        cl: Some(floor),
        meta: meta_delta(meta.next_id.0, meta.tip),
    };
    step.commit(&tail, Durability::Durable).unwrap();
    // The Buffered reorg at 80 to B110, and the wallet durably there.
    sys.header_step(80, 30, 1).unwrap();
    let b = sys.chain().unwrap();
    assert_eq!((b.len(), b[80], b[81] == a[81]), (111, a[80], false));
    sys.sub_step(Sub::Wallet, 110).unwrap();
    sys.wallet.sync_wal().unwrap();
    assert_eq!(sys.image().unwrap().locs[3], Some((b[110], 110)));
    drop(sys);

    world.crash_in_place(CrashKind::PowerLoss, Box::new(LoseStorePending));
    let (_, wallet) = engines(&world, &cfg);
    let sys = System::open(&world, wallet, &cfg).unwrap();
    let m = sys.store.chain_meta().unwrap().unwrap();
    assert_eq!(m.cl, floor, "the durable floor is A70");
    // SB14 D7: the reorg's lowering ended Durable (its last batch), so the chain is A0..A80,
    // with A81..A100 kept off-chain; B's Buffered activation is lost.
    assert_eq!(sys.store.by_id(m.tip).unwrap().unwrap().hash, BlockHash::from(a[80]));
    assert_eq!(sys.chain().unwrap(), a[..=80], "the active chain is A0..A80");
    for x in &b[81..] {
        assert_eq!(id(&sys, x), None, "no B record survives");
    }
    let img = sys.image().unwrap();
    assert_eq!(img.records.len(), 101, "A0..A100 only (A81.. off-chain)");
    assert_eq!(check_stores(&img), []);
    assert_eq!(img.locs[3], Some((a[69], 69)), "E3: the locator's first entry on A");
    assert_eq!(img.paths[3], a[..=69]);
}

// SB14 §3 "Crash tests". Each scenario crashes at every call of its workload, as a process kill
// and as a power cut (sector granularity: torn slot writes), and with `Nested::Depth1` again
// during the recovery from each kill: a kill, a reopen that acts (D17's repairs, the rewinds,
// the anchor step) and a cut (r3 N3).

fn sb14(name: &'static str, headers: HeaderPath, workload: &str, skip: Skip) -> Scenario<StoreSut> {
    let mut sc = Scenario::new(
        name,
        Config {
            headers,
            skip,
            ..Config::default()
        },
        ops(workload),
    );
    sc.nested = Nested::Depth1;
    sc
}

fn clean(sc: &Scenario<StoreSut>) {
    let r = explore(sc, EXHAUSTIVE);
    eprintln!("{}: {}", sc.name, r.summary());
    assert!(r.is_clean(), "{}: {}", sc.name, r.summary());
    assert!(r.r7a.converged > 0, "{}: {}", sc.name, r.summary());
}

/// Raises only (the legacy appends write no slot): a crash after each `pwrite`, after the
/// `sync_data`s, after the directory syncs, inside and after each slot write. Four raises
/// alternate A, B, A, B, so a slot is torn while the other holds the newest file `seq`, and the
/// first-ever raise is torn too (it leaves an empty store; open truncates its segments). The
/// last batch crosses segments (`PER` = 4), creating files whose entries step 3 syncs.
#[test]
fn raises_are_clean_at_every_crash_point() {
    clean(&sb14("sb14-raise", HeaderPath::Legacy, "h3 b h2 b h3 b h6 b", Skip::default()));
}

/// A lowering of three batches (`LOWER_BATCH` = 3) under subscribers on the old branch, then
/// its activation (S1, id uniqueness and each record in one place on every image):
/// - a crash after each batch, and after batch 1 then a kill, reopen and cut (r4 R1 schedule A);
/// - a Buffered earlier batch lost, and a Buffered lowering lost under a durable file write
///   (r2 M4: the old branch's subscriber rows are rewound in the first batch);
/// - between the activation's file sync and the commit that names it (r2 M3).
#[test]
fn a_three_batch_lowering_is_clean() {
    clean(&sb14(
        "sb14-lower3",
        HeaderPath::Step,
        "h9 f9 k9 w9 b h1 r1+3:1 f9 w9 b h1",
        Skip::default(),
    ));
}

/// A reorg above the committed tip (r3 N1, r4 R1 schedule B): the legacy appends leave the
/// in-memory tip above the slot, so the lowering pre-syncs first; a cut after its Durable batch,
/// or after the pre-sync with step 3's syncs Buffered, finds a durable slot whose records, and
/// every copy's parent, are stored.
#[test]
fn a_lowering_above_the_committed_tip_is_clean() {
    clean(&sb14("sb14-above", HeaderPath::Legacy, "h4 f4 b h4 w9 t2 r2+3:1 f9 b", Skip::default()));
}

/// The first lowering on a header-only store creates SQLite first (D4): a crash inside the
/// creation (condition 4) while a file slot is live, and after it.
#[test]
fn the_first_lowering_on_a_header_only_store_is_clean() {
    clean(&sb14("sb14-first-lower", HeaderPath::Legacy, "h5 b t2 r2+2:1 b", Skip::default()));
}

/// The negative controls: with a sync removed, the harness finds the failure.
fn detected(sc: Scenario<StoreSut>, expect: InvariantId) {
    let r = explore(
        &sc,
        Mode::Witness {
            expect: vec![expect],
            max_branches: 200_000,
        },
    );
    assert!(r.found_all, "{}: {expect:?} not found: {}", sc.name, r.summary());
}

/// Step 3 removed: a new segment's entry is lost under a durable slot naming it.
#[test]
fn a_raise_without_its_directory_sync_is_detected() {
    let skip = Skip {
        dir_sync: true,
        ..Skip::default()
    };
    detected(sb14("sb14-no-dirsync", HeaderPath::Legacy, "h3 b h6 b", skip), InvariantId::S1);
}

/// D4's storage-directory sync removed: `spv/` lost after a Durable first lowering, and the stale
/// file slot trusted over segments the lowering cut (r3 N2).
#[test]
fn a_creation_without_its_storage_directory_sync_is_detected() {
    let skip = Skip {
        storage_sync: true,
        ..Skip::default()
    };
    detected(
        sb14("sb14-no-storagesync", HeaderPath::Legacy, "h5 b t2 r2+2:1 b", skip),
        InvariantId::S1,
    );
}

/// The file raise torn while the SQLite slot is newer: a truncation (a SQLite slot), appends
/// over it, then file raises A and B, each crashed inside its write (r2 M1, r3 n2).
#[test]
fn torn_slots_beside_a_newer_sqlite_slot_are_clean() {
    clean(&sb14("sb14-torn", HeaderPath::Legacy, "h3 b h2 b t3 r3+2:1 h2 b h3 b", Skip::default()));
}

/// The process dies once armed: after the `n`-th (0-based) next event of `store` and `op` whose
/// detail (an fs path) ends with `suffix`, every call fails, as after a kill.
#[derive(Clone, Default)]
struct Killer(Arc<std::sync::Mutex<Option<KillAt>>>);

/// The store, the operation, the detail's suffix and the matches left to skip.
type KillAt = (&'static str, OpKind, &'static str, u32);

impl Killer {
    fn world(&self) -> World {
        let k = self.clone();
        World::new(WorldConfig {
            on_event: Some(Box::new(move |w: &World, _| k.after(w))),
            ..WorldConfig::default()
        })
    }

    fn arm(&self, store: &'static str, op: OpKind, suffix: &'static str, n: u32) {
        *self.0.lock().unwrap() = Some((store, op, suffix, n));
    }

    fn after(&self, w: &World) {
        let mut armed = self.0.lock().unwrap();
        let Some((store, op, suffix, n)) = armed.as_mut() else {
            return;
        };
        let trace = w.trace();
        let Some(dash_kv::faulty::TraceEntry::Event {
            store: s,
            op: o,
            detail,
            ..
        }) = trace.entries.last()
        else {
            return;
        };
        // A path's separator is `\` on Windows: suffixes are written with `/`.
        if s == store && o == op && detail.replace('\\', "/").ends_with(*suffix) {
            match n.checked_sub(1) {
                Some(left) => *n = left,
                None => {
                    w.freeze();
                    *armed = None;
                }
            }
        }
    }
}

/// Every pending write lost at a power cut, in both databases and the files.
struct LoseAll;

impl Decider for LoseAll {
    fn choose(&mut self, var: &Var) -> u32 {
        var.domain - 1
    }
}

fn legacy() -> Config {
    Config {
        headers: HeaderPath::Legacy,
        ..Config::default()
    }
}

fn reopen(world: &World, cfg: &Config) -> System {
    let (_, wallet) = engines(world, cfg);
    System::open(world, wallet, cfg).unwrap()
}

/// r4 R1 schedule A (r3 N3): a Buffered deep truncation killed before its Durable batch, then
/// reopened, then cut. Open's Durable repair makes the winning (Buffered) batch slot durable
/// before it truncates the files, so no committed header is lost: A0..A9 all stay stored, the
/// lowered ones off-chain, every parent stored, and no row names a height above the marker.
#[test]
fn schedule_a_a_killed_truncation_reopened_then_cut() {
    let kill = Killer::default();
    let world = kill.world();
    let cfg = legacy();
    let sys = reopen(&world, &cfg);
    sys.header_step(u32::MAX, 9, 0).unwrap();
    sys.sub_step(Sub::Store(Subscriber::Filters), 9).unwrap();
    sys.boundary().unwrap();
    let a = sys.chain().unwrap();
    // Heights 2..=9 lower in three batches (3, 3, 2): killed after the second, Buffered one.
    kill.arm("spv", OpKind::Append(CommitKind::Buffered), "", 1);
    assert!(sys.store.truncate_headers(1).is_err(), "the process died");
    drop(sys);
    world.crash_in_place(CrashKind::ProcessKill, Box::new(LoseAll));
    let sys = reopen(&world, &cfg);
    let tip = sys.chain().unwrap().len() - 1;
    assert_eq!(tip, 3, "the second batch's slot, at b − 1");
    drop(sys);
    world.crash_in_place(CrashKind::PowerLoss, Box::new(LoseAll));
    let sys = reopen(&world, &cfg);
    let img = sys.image().unwrap();
    assert_eq!(check_stores(&img), []);
    assert_eq!(img.active, a[..=tip], "the repaired slot survived the cut");
    for x in &a {
        assert!(img.records.contains_key(x), "a committed header was lost");
    }
    assert!(img.locs[1].is_none_or(|(_, h)| h <= tip as u32));
}

/// r4 R1 schedule B (r3 N3): a file raise killed before its `sync_data`, then reopened, then a
/// Durable subscriber commit naming its heights, then cut. Open's `sync_data` of the winning
/// file slot makes it durable before anything names it.
#[test]
fn schedule_b_a_killed_raise_reopened_named_then_cut() {
    let kill = Killer::default();
    let world = kill.world();
    let cfg = legacy();
    let sys = reopen(&world, &cfg);
    sys.header_step(u32::MAX, 5, 0).unwrap();
    sys.boundary().unwrap();
    sys.header_step(u32::MAX, 4, 0).unwrap();
    let a = sys.chain().unwrap();
    kill.arm(dash_kv::faulty::FS_STORE, OpKind::WriteAt, "/tip", 0);
    assert!(sys.store.raise().is_err(), "the process died before the slot's sync_data");
    drop(sys);
    world.crash_in_place(CrashKind::ProcessKill, Box::new(LoseAll));
    let sys = reopen(&world, &cfg);
    assert_eq!(sys.chain().unwrap(), a, "the killed raise's slot is visible");
    sys.sub_step(Sub::Store(Subscriber::Filters), 9).unwrap();
    sys.boundary().unwrap();
    drop(sys);
    world.crash_in_place(CrashKind::PowerLoss, Box::new(LoseAll));
    let sys = reopen(&world, &cfg);
    let img = sys.image().unwrap();
    assert_eq!(check_stores(&img), []);
    assert_eq!(img.active, a, "the slot open synced survived the cut");
    assert_eq!(img.locs[1], Some((a[9], 9)));
}

/// D18: a failed `sync_data` poisons the writer. The raise fails, the next one fails without
/// writing a slot, lookups keep working, and a reopen recovers the last durable slot.
#[test]
fn a_sync_error_poisons_the_writer() {
    let world = World::new(WorldConfig {
        faults: vec![Fault {
            at: EventSel::Nth {
                store: StoreSel::Fs,
                op: OpKind::SyncData,
                n: 2,
            },
            kind: FaultKind::Io,
            outcome: FaultOutcome::Applied,
        }],
        ..WorldConfig::default()
    });
    let cfg = legacy();
    let sys = reopen(&world, &cfg);
    let mut syncs = 0;
    let mut raised = sys.chain().unwrap();
    // Raise until the injected failure: each raise syncs a segment and `tip`.
    for _ in 0..4 {
        sys.header_step(u32::MAX, 1, 0).unwrap();
        match sys.boundary() {
            Ok(()) => {
                syncs += 1;
                raised = sys.chain().unwrap();
            }
            Err(_) => break,
        }
    }
    assert!(world.fault_fired(), "the sync failed after {syncs} raises");
    let chain = sys.chain().unwrap();
    assert!(matches!(sys.store.raise(), Err(Error::Poisoned(_))), "the writer is poisoned");
    assert_eq!(sys.chain().unwrap(), chain, "lookups keep working");
    let events = world.trace().events().count();
    assert!(sys.store.raise().is_err());
    assert_eq!(world.trace().events().count(), events, "a poisoned raise writes nothing");
    drop(sys);
    world.crash_in_place(CrashKind::PowerLoss, Box::new(LoseAll));
    let sys = reopen(&world, &cfg);
    assert_eq!(sys.chain().unwrap(), raised, "the last durable slot");
}

/// D18 on the append path: a segment `pwrite` that fails during an append's eviction latches.
/// The append fails, every later raise is refused with no slot written, and a reopen after a
/// power cut recovers the last durable slot.
#[test]
fn a_failed_append_write_latches() {
    let world = World::new(WorldConfig {
        faults: vec![Fault {
            at: EventSel::Nth {
                store: StoreSel::Fs,
                op: OpKind::WriteAt,
                n: 2,
            },
            kind: FaultKind::Io,
            outcome: FaultOutcome::NotApplied,
        }],
        ..WorldConfig::default()
    });
    let cfg = legacy();
    let sys = reopen(&world, &cfg);
    let raised = sys.chain().unwrap();
    // Twelve headers fill three segments (`PER` = 4): the third evicts the first, whose write
    // fails.
    assert!(sys.header_step(u32::MAX, 12, 0).is_err(), "the append's write failed");
    assert!(world.fault_fired());
    assert!(matches!(sys.store.raise(), Err(Error::Poisoned(_))));
    let events = world.trace().events().count();
    assert!(sys.store.sync_wal().is_err());
    assert_eq!(world.trace().events().count(), events, "a latched store writes nothing");
    drop(sys);
    world.crash_in_place(CrashKind::PowerLoss, Box::new(LoseAll));
    let sys = reopen(&world, &cfg);
    assert_eq!(sys.chain().unwrap(), raised, "the last durable slot");
}

/// D18 on the lookup path: a lookup that evicts an unsynced segment's fd (`FD_CAP` = 32), whose
/// `sync_data` fails, latches and still answers; the next raise refuses, and nothing retries the
/// sync.
#[test]
fn a_failed_eviction_sync_on_a_lookup_latches() {
    let cfg = legacy();
    let fs_syncs = |world: &World| {
        world
            .trace()
            .events()
            .filter(|(_, s, op, _)| *s == dash_kv::faulty::FS_STORE && *op == OpKind::SyncData)
            .count()
    };
    // 140 headers fill 35 segments (`PER` = 4): their fds evict, and the cache ends on unsynced
    // ones. A dry run counts the syncs before the lookup.
    let before = {
        let world = World::new(WorldConfig::default());
        let sys = reopen(&world, &cfg);
        sys.header_step(u32::MAX, 140, 0).unwrap();
        fs_syncs(&world)
    };
    let world = World::new(WorldConfig {
        faults: vec![Fault {
            at: EventSel::Nth {
                store: StoreSel::Fs,
                op: OpKind::SyncData,
                n: before as u32,
            },
            kind: FaultKind::Io,
            outcome: FaultOutcome::Applied,
        }],
        ..WorldConfig::default()
    });
    let sys = reopen(&world, &cfg);
    sys.header_step(u32::MAX, 140, 0).unwrap();
    assert!(!world.fault_fired());
    let chain = sys.chain().expect("the lookup answers");
    assert_eq!(chain.len(), 141);
    assert!(world.fault_fired(), "the segment-0 lookup evicted an unsynced fd");
    let syncs = fs_syncs(&world);
    assert!(matches!(sys.store.raise(), Err(Error::Poisoned(_))));
    sys.chain().expect("lookups keep working");
    assert_eq!(fs_syncs(&world), syncs, "no sync runs once one failed");
}

/// Sol r1 #2: `persist` (`sync_wal`) is durable when a fast append started a run after a
/// lowering, where a plain raise commits a Buffered SQLite slot. A0..A5 durable, lowered to A2,
/// B3..B4 appended, persisted, cut: B4 survives. The same schedule with `raise` reopens at A2.
#[test]
fn persist_survives_a_cut_after_a_new_run() {
    for persist in [true, false] {
        let world = World::new(WorldConfig::default());
        let cfg = legacy();
        let sys = reopen(&world, &cfg);
        sys.header_step(u32::MAX, 5, 0).unwrap();
        sys.store.sync_wal().unwrap();
        let a = sys.chain().unwrap();
        assert_eq!(a.len(), 6);
        sys.store.truncate_headers(2).unwrap();
        sys.header_step(u32::MAX, 2, 1).unwrap();
        let b = sys.chain().unwrap();
        assert_eq!((b.len(), &b[..3]), (5, &a[..3]));
        if persist {
            sys.store.sync_wal().unwrap();
        } else {
            sys.store.raise().unwrap();
        }
        drop(sys);
        world.crash_in_place(CrashKind::PowerLoss, Box::new(LoseAll));
        let sys = reopen(&world, &cfg);
        match persist {
            true => assert_eq!(sys.chain().unwrap(), b, "persisted B3..B4"),
            false => assert_eq!(sys.chain().unwrap(), a[..3], "a Buffered raise is lost"),
        }
    }
}

/// D7: a reorg (or a legacy truncation and its branch) whose first headers are already active
/// lowers from its fork all the same, and rewinds the store's subscribers to it: recovery cannot
/// put them back on the re-activated records. Sweeps 0x5b14_2601, _2603 and _2604 found the
/// reference expecting them there.
#[test]
fn a_lowering_that_reactivates_its_records_rewinds_the_subscribers() {
    for (headers, workload, crash, decide) in [
        ("step", "h2;r0+3:2;f4;b;f3;r0+1:2;h1;r1+2:1;h2", "38 process-kill", ""),
        (
            "legacy",
            "h1;t0;r0+3:1;b;b;c4;t0;r0+1:1;b;h1;t1;r1+2:2;t1;r1+3:3;t2;r2+1:3",
            "36 power-loss",
            "",
        ),
        (
            "legacy",
            "w1;c0;h3;t1;r1+3:3;f3;k1;t2;r2+1:3;f3;b",
            "34 power-loss",
            "decide 1:wallet:prefix=1",
        ),
    ] {
        let r = dash_kv::faulty::Repro::parse(&format!(
            "harness st3a/1
             sut sb13-spv-store
             config engine=prefix bug=none headers={headers}
             workload {workload}
             crash {crash}
             {decide}
             adversary realistic
             granularity sector"
        ))
        .unwrap();
        let v = replay::<StoreSut>(&r).unwrap();
        assert!(v.is_empty(), "{workload}: {v:#?}");
    }
}

/// D42: after an acknowledged raise, a lowering commits a lower marker, and a fast append above
/// it is unraised again: a process kill may lose it down to that marker while a later
/// subscriber commit survives. Sweep 0x5b14_2102 found the oracle rejecting this state.
#[test]
fn a_cut_down_to_a_later_lowering_is_a_state() {
    let r = dash_kv::faulty::Repro::parse(
        "harness st3a/1
         sut sb13-spv-store
         config engine=prefix bug=none headers=legacy
         workload b;h3;c1;b;t1;r1+3:2;k0;t2;r2+1:1;h2;b;w6
         crash 24 process-kill",
    )
    .unwrap();
    let v = replay::<StoreSut>(&r).unwrap();
    assert!(v.is_empty(), "{v:#?}");
}
