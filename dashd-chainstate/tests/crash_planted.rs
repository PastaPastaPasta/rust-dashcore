//! CH2's planted bugs on the CRASH model (§6.4, §7.2): ST3a's T-bugs re-planted in W and
//! recovery, Opus's CH2 additions that need the chainstate, FA7, and the controls. Each witnessed
//! bug has a bounded search that must find the invariants it breaks, and a checked-in repro under
//! `tests/crash/regress` that replays it (`regenerate_planted_repros`, ignored, rewrites them).
//! The coins-only additions (X9, X10, X13, X29–X32, X34–X36) stay with the cache's own tests.

#[path = "crash/common.rs"]
mod common;
use common::*;
use dashd_chainstate::Planted;
use dashd_chainstate::crash::coins_replay_blind;
use dashd_store::faulty::{
    Decider, FS_STORE, FatalCause, Seeded, Var, World, WorldConfig, minimize, replay_trace,
    reproduces, sweep,
};

const REGRESS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/crash/regress");

/// S4's branches per image, at most, unplanted (4 when calibrated).
const S4_BRANCH_BOUND: u64 = 8;

fn with(mut sc: Sc, bug: Planted) -> Sc {
    sc.cfg.planted = Some(bug);
    sc
}

fn sc(name: &'static str, cfg: &str, workload: &str) -> Sc {
    Scenario::new(name, cfg.parse().expect("a config"), ops(workload))
}

fn process_kill(mut sc: Sc) -> Sc {
    sc.kinds = vec![CrashKind::ProcessKill];
    sc
}

fn power_loss(mut sc: Sc) -> Sc {
    sc.kinds = vec![CrashKind::PowerLoss];
    sc
}

/// S5 with an `Applied` failure of Bk.
fn s5_bk_fault() -> Sc {
    let mut sc = from_selection(s5());
    sc.faults = FaultPlan(vec![bk_fault(&s5())]);
    sc
}

/// A coin fetch of `c10;f;c2`'s block 11 fails (chainstate reads 0–17 are the load's).
fn coin_read_fault() -> Fault {
    io(chain(), OpKind::Read, 18, FaultOutcome::NotApplied)
}

fn coin_read() -> Sc {
    let mut sc = sc("coin-read", "", "c10;f;c2");
    sc.faults = FaultPlan(vec![coin_read_fault()]);
    sc
}

/// Recovery's first block read (S4's replay) fails: block-file read 1, after the load's one open
/// per blk file the index holds, here blk00000.dat (BL-127).
fn replay_read() -> Sc {
    let mut sc = s4();
    sc.name = "replay-read";
    sc.recovery_faults = vec![io(StoreSel::Fs, OpKind::Read, 1, FaultOutcome::NotApplied)];
    sc
}

/// S4 with an `Applied` failure of the files barrier's first `sync_data`.
fn s4_sync() -> Sc {
    let mut sc = s4();
    sc.faults = FaultPlan(vec![io(StoreSel::Fs, OpKind::SyncData, 0, FaultOutcome::Applied)]);
    sc
}

/// A witnessed bug: its scenario (unplanted), built only when asked for, as some run the forward
/// model, and the invariants it breaks, the first being its repro's.
type Witnessed = (Planted, fn() -> Sc, Vec<InvariantId>);

/// Every witnessed bug.
fn witnessed() -> Vec<Witnessed> {
    use Planted::*;
    vec![
        (T1, || at_flush(s1(), 1), vec![R1, D1a]),
        (T2, || at_flush(s1(), 0), vec![R1, D1a]),
        (T3, || s4(), vec![R3, R4]),
        (T4, || s4(), vec![R2]),
        // The barrier after a rollover, with rev00000 lagging behind blk00001.
        (T6, || at_flush(sc("T6", "", "c120;f;a10;c1;f"), 1), vec![R1, D1a]),
        (T7, || s4_nested(), vec![R2, R7a]),
        (T8, || at_flush(sc("T8", "sub=250", "c10;f;r7:3;f"), 1), vec![R6, R3]),
        (T9, || s4_sync(), vec![R8]),
        (T14, || at_flush(s1(), 1), vec![R1, R12]),
        (T17, || s4(), vec![R3, R4]),
        (T18, || s4(), vec![R3, D1f]),
        (T20, || s7(), vec![R6, R3]),
        // R0: V's recovery refuses the unapplied tip rather than replaying it wrong.
        (T21, || s7(), vec![R2, R0]),
        (T24, || process_kill(s4()), vec![R2]),
        (T26, || s4(), vec![R3, D1f]),
        (T27, || s4_nested(), vec![D1f, R3, R7a]),
        // R12 with D1f: the missing barrier loses the presumed effect, not only the trace's shape.
        (T29, || at_flush(s1(), 2), vec![D1f, R12]),
        (T31, || at_flush(s1(), 1), vec![R5, R12]),
        (T34, || at_flush(s4b(), 1), vec![R3, D1f]),
        // Pruning (b2): steps 7, 10 and 12.
        (T5, || from_selection(s5()), vec![R0, D1e]),
        (T11, || power_loss(from_unlink(s5())), vec![R0, D1e]),
        (T13, || s5(), vec![R0, D1b]),
        (T19, || s5b(), vec![R0, D1b]),
        (T25, || s5_bk_fault(), vec![R8, D1e]),
        // FA2 on the forward run.
        (T39, || s14b(), vec![FA2]),
        (T40, || s14b(), vec![FA2]),
        (X6, || process_kill(s4()), vec![R12]),
        // X14 counts S1's presumed effect at 25 covered once flush 1 is durable, so flush 2 skips
        // its presumed barrier (D-c6-7: why not an effect written inside an `F d`).
        (X14, || at_flush(s1(), 2), vec![D1f, R12]),
        // FA7, which the harness reports as R8 ("FA7: …").
        (X16a, || coin_read(), vec![R8]),
        (X16b, || coin_read(), vec![R8]),
        (X21, || sc("corrupt-coin", "", "c10;f;K;c1"), vec![R8]),
        // DEC-185 (3)'s negative: an unmarked refusal after a fatal error.
        (Unmarked, || replay_read(), vec![R8]),
        // C9's control (m2): a roll that never comes up.
        (NoRoll, || c9_replay(3), vec![Model]),
        // DEC-202's negative: S1's effect at 5 with no durable tip, refused as corruption.
        (NoTipEmpty, || s1_early(), vec![R0]),
        // Sol r1's probe on an ordinary restart: the control of its refusal witness.
        (LostEffect, || sc("lost-effect", "effects=5", "c5;f;i"), vec![R13]),
        // Sol r2's control: the coin `RefuseCoin` deletes, on every open.
        (LostCoin, || sc("lost-coin", "", "c5;f;i"), vec![R3]),
    ]
}

/// The connect halves (CH2 §7.4 rev2 F5/n1, CH3a a3): bugs witnessed through the real
/// `ConnectBlock` (core mode), each with a checked-in repro, `<bug>-core`. T12 is planted in the
/// core itself; T20 and T21 are W's, on S7-core, where activation's real connect finds B invalid.
fn connect_halves() -> Vec<Witnessed> {
    use Planted::*;
    vec![
        // A flush between the connect and the deferred write names a record the files lack.
        (T12, || s1_core(), vec![R1, D1a]),
        (T20, || s7_core(), vec![R6, R3]),
        (T21, || s7_core(), vec![R2, R0]),
    ]
}

fn connect_half_of(bug: Planted) -> Repro {
    let (_, sc, expect) =
        connect_halves().into_iter().find(|(b, ..)| *b == bug).expect("a connect half");
    witness(bug, sc(), &expect)
}

/// The bugs witnessed under DEC-196's warranted refusal (`x_refused`), with their configuration
/// and the rule the refusal's R0 must name. Each has a checked-in repro, `<bug>-refused`.
fn refusal_witnessed() -> Vec<(Planted, &'static str, &'static str)> {
    use Planted::*;
    vec![
        (T31, "", "R5"),
        (LostEffect, "effects=5", "R13"),
        (RefuseCoin, "", "R3"),
        (RefuseHead, "", "R2"),
        (RefuseEvo, "", "R2"),
    ]
}

/// DEC-196's warranted refusal: the coins at block 5, and the index-only write after `x3` marks
/// it failed.
fn x_refused(cfg: &str) -> Sc {
    let mut s = sc("x-refused", cfg, "c5;f;x3;i");
    // From the flush's B1 on, so the image after the index-only write's append is in it.
    s.window = CrashWindow {
        from: after(MarkKind::Durable, 0),
        to: None,
    };
    s
}

/// Whether `v` holds the refusal R0 that names a violation of `rule`.
fn refused_naming(v: &[Violation], rule: &str) -> bool {
    v.iter().any(|v| {
        v.invariant == R0
            && v.detail.contains("recovered stores fail")
            && v.detail.contains(&format!("invariant: {rule},"))
    })
}

/// `bug`'s witness under the refusal: the window is clean unplanted, and planted its refusal's R0
/// names the rule (`refusal_witnessed`).
fn refused_witness(bug: Planted) -> Repro {
    let (_, cfg, rule) =
        refusal_witnessed().into_iter().find(|(b, ..)| *b == bug).expect("a refusal witness");
    clean(&x_refused(cfg));
    let r = witness(bug, x_refused(cfg), &[R0]);
    let v = replay::<ChainSut>(&r).expect("a repro");
    assert!(refused_naming(&v, rule), "{bug}: {v:?}");
    r
}

/// A witness search for `expect` on `sc` planted with `bug`: every invariant found, and the first
/// one's repro replays it.
fn witness(bug: Planted, sc: Sc, expect: &[InvariantId]) -> Repro {
    let sc = with(sc, bug);
    let r = explore(
        &sc,
        Mode::Witness {
            expect: expect.to_vec(),
            max_branches: 20_000,
        },
    );
    eprintln!("{bug}: {}", r.summary());
    assert!(r.found_all, "{bug}: expected {expect:?}\n{}", r.summary());
    let f = r.first(expect[0]).expect("found");
    let got = replay::<ChainSut>(&f.repro).expect("a repro");
    assert!(got.iter().any(|v| v.invariant == expect[0]), "{bug}: the repro replays: {got:?}");
    f.repro.clone()
}

fn witness_of(bug: Planted) -> Repro {
    let (_, sc, expect) =
        witnessed().into_iter().find(|(b, ..)| *b == bug).expect("a witnessed bug");
    witness(bug, sc(), &expect)
}

#[test]
fn t1_b1_before_the_files_barrier() {
    witness_of(Planted::T1);
}

#[test]
fn t2_index_write_without_the_files_barrier() {
    witness_of(Planted::T2);
}

#[test]
fn t3_marker_in_bk() {
    witness_of(Planted::T3);
}

#[test]
fn t4_evo_in_bk() {
    witness_of(Planted::T4);
}

#[test]
fn t6_barrier_syncs_only_the_current_files() {
    witness_of(Planted::T6);
}

#[test]
fn t7_recovery_erases_the_marker_first() {
    witness_of(Planted::T7);
}

#[test]
fn t8_forward_leg_reads_the_old_branch() {
    witness_of(Planted::T8);
}

#[test]
fn t9_w_continues_after_a_failed_sync() {
    witness_of(Planted::T9);
}

#[test]
fn t14_b1_drops_the_file_records() {
    witness_of(Planted::T14);
}

#[test]
fn t17_b1_buffered() {
    witness_of(Planted::T17);
}

#[test]
fn t18_bk_buffered_without_its_barrier() {
    witness_of(Planted::T18);
}

#[test]
fn t20_split_flush_names_an_unconnected_evo_tip() {
    witness_of(Planted::T20);
}

#[test]
fn t21_marker_names_an_unconnected_block() {
    witness_of(Planted::T21);
}

#[test]
fn t12_undo_flag_before_the_record() {
    connect_half_of(Planted::T12);
}

#[test]
fn t20_core_split_flush_names_an_unconnected_evo_tip() {
    connect_half_of(Planted::T20);
}

#[test]
fn t21_core_marker_names_an_unconnected_block() {
    connect_half_of(Planted::T21);
}

#[test]
fn t24_evo_after_bk() {
    witness_of(Planted::T24);
}

#[test]
fn t26_bk_durable_without_its_barrier() {
    witness_of(Planted::T26);
}

#[test]
fn t27_recovery_final_batch_without_its_barrier() {
    witness_of(Planted::T27);
}

#[test]
fn t29_b1_without_the_presumed_barrier() {
    witness_of(Planted::T29);
}

#[test]
fn t31_recovery_erases_the_tip_record() {
    witness_of(Planted::T31);
}

/// DEC-196's warranted refusal still judges what recovery did: with T31, recovery erases the
/// tip's record and V then refuses to start (the loaded tip failed and outworks every valid
/// block). R0 reports the refusal, naming the damage.
#[test]
fn t31_under_a_warranted_refusal() {
    refused_witness(Planted::T31);
}

#[test]
fn lost_effect_on_an_ordinary_restart() {
    witness_of(Planted::LostEffect);
}

/// R13 judges a warranted refusal too (Sol r1): an effect a completed barrier covered, lost after
/// recovery, fails the refusal's checks, so R0 reports the refusal, naming R13. The ordinary
/// restart (`lost_effect_on_an_ordinary_restart`) is its control.
#[test]
fn lost_effect_under_a_warranted_refusal() {
    refused_witness(Planted::LostEffect);
}

#[test]
fn lost_coin_on_an_ordinary_restart() {
    witness_of(Planted::LostCoin);
}

/// The refusal is judged as an open, but for activation's outcome (Sol r2): a coin lost only when
/// E requires the refusal is never lost on the image before the index-only write, which opens
/// clean, so only the refusal's own R3 finds it. `lost_coin_on_an_ordinary_restart` is its
/// control.
#[test]
fn a_coin_lost_only_under_a_refusal() {
    clean(&with(sc("pre-index", "", "c5;f;x3"), Planted::RefuseCoin));
    refused_witness(Planted::RefuseCoin);
}

/// The recovered R2 clauses on the refusal: a `head_blocks` marker left, or no `evo_tip`.
#[test]
fn recovered_markers_under_a_refusal() {
    refused_witness(Planted::RefuseHead);
    refused_witness(Planted::RefuseEvo);
}

#[test]
fn t34_bk_buffered_behind_its_barrier() {
    witness_of(Planted::T34);
}

#[test]
fn t5_unlink_before_bk() {
    witness_of(Planted::T5);
}

#[test]
fn t11_buffered_bk_then_the_unlink() {
    witness_of(Planted::T11);
}

#[test]
fn t13_selection_before_b1_clears_in_b1() {
    witness_of(Planted::T13);
}

#[test]
fn t19_selection_before_b1_omits_the_modified_records() {
    witness_of(Planted::T19);
}

#[test]
fn t25_unlink_after_a_failed_bk() {
    witness_of(Planted::T25);
}

#[test]
fn t39_the_automatic_selector_rerun() {
    witness_of(Planted::T39);
}

#[test]
fn t40_no_rearm_after_a_shortfall() {
    witness_of(Planted::T40);
}

#[test]
fn x6_no_sync_wal_at_open() {
    witness_of(Planted::X6);
}

#[test]
fn x14_late_effects_counted_as_covered() {
    witness_of(Planted::X14);
}

#[test]
fn x16a_a_failed_read_as_a_missing_coin() {
    fa7(witness_of(Planted::X16a));
}

#[test]
fn x16b_the_guard_swallows_a_read_error() {
    fa7(witness_of(Planted::X16b));
}

#[test]
fn x21_a_corrupt_coin_as_a_missing_one() {
    fa7(witness_of(Planted::X21));
}

/// FA7's R8: the repro's run judged or connected after its failed read.
fn fa7(r: Repro) {
    let got = replay::<ChainSut>(&r).expect("a repro");
    assert!(got.iter().any(|v| v.invariant == R8 && v.detail.starts_with("FA7: ")), "{got:?}");
}

/// DEC-185 (3): a refusal after a fatal error without V's marks fails R8; with them, the same
/// failed replay read is clean.
#[test]
fn unmarked_refusal_fails_r8() {
    clean(&replay_read());
    witness_of(Planted::Unmarked);
}

/// Round 1's m2: with the roll disarmed, the check that it came up fails at every replay point,
/// so C9's recovery half is not vacuous.
#[test]
fn c9_replay_check_fails_without_the_roll() {
    for k in 0..9 {
        let sc = with(c9_replay(k), Planted::NoRoll);
        let r = explore(&sc, EXHAUSTIVE);
        assert!(
            r.findings.iter().any(|f| f.violation.invariant == Model),
            "k={k}: {}",
            r.summary()
        );
    }
}

#[test]
fn no_tip_effects_counted_as_corruption() {
    witness_of(Planted::NoTipEmpty);
}

/// X8: the replay reading each coins key before writing it adds a read per key, each a decision
/// over the pending flush: S4's branches per image grow past the bound the unplanted search keeps.
#[test]
fn x8_replay_reads_blow_the_branch_bound() {
    let clean = explore(&s4(), EXHAUSTIVE);
    assert!(clean.is_clean(), "{}", clean.summary());
    assert!(clean.max_branches_per_image <= S4_BRANCH_BOUND, "{}", clean.summary());
    let x8 = explore(&with(s4(), Planted::X8), EXHAUSTIVE);
    assert!(x8.max_branches_per_image > S4_BRANCH_BOUND, "{}", x8.summary());
}

/// FA7's unplanted halves: a failed coin read and a corrupt coin stop the run with Core's message,
/// nothing connected or judged after it.
#[test]
fn fa7_holds_unplanted() {
    let (_, stopped) = faulted(&coin_read(), coin_read_fault());
    assert_eq!(stopped.as_deref(), Some("Error reading from database, shutting down."));
    let v = replay::<ChainSut>(&Repro::new(&coin_read(), Some(coin_read_fault())));
    assert_eq!(v.expect("a repro"), []);
    // The corrupt coin stops `c1` (DEC-78): the forward run's only report is that refusal.
    let (_, v) = forward_trace(&sc("corrupt-coin", "", "c10;f;K;c1"));
    let refused = "op 4 refused: Error reading from database, shutting down.";
    assert!(matches!(&v[..], [p] if p.invariant == Precondition && p.detail == refused), "{v:?}");
}

/// FA7 starts where V reads the bad bytes: not at a corrupt coin the next flush rewrites (dirty in
/// the cache, or restored by disconnecting a spend the cache served), and a fault V tolerates (an
/// `Allocate`'s) does not move the op that met the start.
#[test]
fn fa7_starts_at_the_read() {
    for w in ["c10;K;f;c1", "c10;K;c1;x11;f;c1"] {
        let (_, v) = forward_trace(&sc("corrupt-coin", "", w));
        assert_eq!(v, [], "{w}");
    }
    for n in [0, 1] {
        let s = sc("corrupt-coin", "", "c10;f;K;c1");
        let f = io(StoreSel::Fs, OpKind::Allocate, n, FaultOutcome::NotApplied);
        let v = replay::<ChainSut>(&Repro::new(&s, Some(f))).expect("a repro");
        assert!(v.iter().all(|v| v.invariant == Precondition), "Allocate {n}: {v:?}");
    }
}

/// A restart whose block file a crash lost (T1's marker before the files barrier, then a power
/// loss that drops blk00000.dat's directory entry) logs Core's open line at the load's blk-file
/// check, which refuses before the replay (BL-127). No failed directory sync lost the file, so the
/// refusal is unwarranted: R0.
#[test]
fn a_lost_block_file_gives_cores_open_lines() {
    let (v, t) = replay_trace::<ChainSut>(&checked_in("T1-lost-file")).expect("a repro");
    assert_eq!(lost_file_lines(&t), LOST_FILE_LINES);
    let unwarranted = |v: &Violation| {
        v.invariant == R0
            && v.detail.contains("Error loading block database (BL-127: /dd/blocks/blk00000.dat")
    };
    assert!(v.iter().any(unwarranted), "{v:?}");
}

const LOST_FILE_LINES: [&str; 1] = ["Unable to open file /dd/blocks/blk00000.dat"];

/// The open lines a trace logs, in order.
fn lost_file_lines(t: &Trace) -> Vec<String> {
    let open = |l: &String| l.starts_with("Unable to open file") || l.contains("OpenBlockFile");
    logs(t, "").into_iter().filter(open).collect()
}

/// The lost-file image: `c2;f` split, T1's B1 durable before the files barrier, and a power loss
/// right after it that drops `/dd/blocks`' second operation (blk00000.dat's create).
fn lost_file_repro() -> Repro {
    let mut r = Repro::new(&with(sc("lost-file", "sub=250", "c2;f"), Planted::T1), None);
    r.crash_at = Some(14);
    r.kind = CrashKind::PowerLoss;
    r.decisions.insert("1:fs:/dd/blocks:op1".into(), 1);
    r.expect = vec![R0];
    r
}

/// Decides every pending operation on `blocks/` lost, and the rest survived.
struct LoseBlocksDir;

impl Decider for LoseBlocksDir {
    fn choose(&mut self, var: &Var) -> u32 {
        u32::from(var.name.contains(":fs:/dd/blocks:op")).min(var.domain - 1)
    }
}

/// `c2;f` with both of the flush's syncs of `blocks/` failed (DEC-212: each logged, the flush goes
/// on, B1 durable), then a power loss that loses `blocks/`' pending creates; the booted world.
/// One fault per run cannot do it: the rev file's sync commits the blk file's create.
fn lost_to_failed_dir_syncs(cfg: &ChainConfig) -> World {
    let dir_sync = |n| io(StoreSel::Fs, OpKind::SyncDir, n, FaultOutcome::NotApplied);
    let world = World::new(WorldConfig {
        // The barrier's syncs: `blocks/`, the datadir, then the same after the rev file's.
        faults: vec![dir_sync(0), dir_sync(2)],
        ..WorldConfig::default()
    });
    let mut sut = ChainSut::open(&world, cfg).expect("the first open");
    for op in ops("c2;f") {
        sut.apply(&op).expect("the forward run");
    }
    drop(sut);
    let failed = |e: &&TraceEntry| {
        matches!(e, TraceEntry::Event { op: OpKind::SyncDir, failed: true, detail, .. }
            if detail == "/dd/blocks")
    };
    assert_eq!(world.trace().entries.iter().filter(failed).count(), 2, "both blocks/ syncs");
    world.capture().boot(CrashKind::PowerLoss, Box::new(LoseBlocksDir), WorldConfig::default())
}

/// BL-127 (Opus r1's m-4 on CH2b b4): a restart whose blk file a failed directory sync lost is
/// refused at the load's blk-file check with Core's "Error loading block database", as CH1-node's
/// load refuses it (Core `node/blockstorage.cpp:377-390`), and the message is sanctioned with no
/// fatal error before it. `NoBlkChk` loads anyway, and R1, which no longer entitles a lost blk
/// file on an open, finds it.
#[test]
fn a_blk_file_lost_to_a_failed_dir_sync_refuses_the_restart() {
    let cfg = ChainConfig::default();
    let booted = lost_to_failed_dir_syncs(&cfg);
    let a = ChainSut::open(&booted, &cfg).err().expect("the load refuses");
    assert_eq!(a.message, "Error loading block database");
    assert!(ChainSut::sanctioned(&cfg, &a.message));
    assert!(!ChainSut::sanctioned(&cfg, "Error loading block database (BL-127: R1)"));
    // `-dbcrashratio`'s exit (`txdb.cpp:159`) only when it is configured.
    assert!(!ChainSut::sanctioned(&cfg, "Simulating a crash. Goodbye."));
    assert_eq!(lost_file_lines(&booted.trace()), LOST_FILE_LINES);
    // The refusal reports no store failure and no abort mark: R0 judges it by its message.
    assert!(!booted.trace().marks().any(|m| matches!(m, Mark::Abort { .. })));
    // A failed read of the check's open is fatal there, and only there is the message Core's.
    use dashd_chainstate::crash::{CHAIN_STORE, PATH_LOAD, PATH_LOAD_INDEX};
    let read_in = |paths: Vec<&'static str>| FatalCause {
        read: true,
        store: Some(FS_STORE.into()),
        paths,
    };
    let check = read_in(vec![PATH_LOAD, PATH_LOAD_INDEX]);
    assert!(ChainSut::sanctioned_after(&cfg, &a.message, &check));
    assert!(!ChainSut::sanctioned_after(&cfg, &a.message, &read_in(vec![PATH_LOAD])));
    // The check only opens: no write there, and no chainstate failure, gives Core's message.
    let write = FatalCause {
        read: false,
        ..check.clone()
    };
    let chainstate = FatalCause {
        store: Some(CHAIN_STORE.into()),
        ..check.clone()
    };
    assert!(!ChainSut::sanctioned_after(&cfg, &a.message, &write));
    assert!(!ChainSut::sanctioned_after(&cfg, &a.message, &chainstate));

    let planted: ChainConfig = "planted=NoBlkChk".parse().expect("a config");
    let booted = lost_to_failed_dir_syncs(&planted);
    let sut = ChainSut::open(&booted, &planted).unwrap_or_else(|a| panic!("loads: {a}"));
    let v = booted
        .checking(|| sut.check(&booted, &ChainRef::checks_view(&planted), &booted.trace()).err());
    let lost = |v: &Violation| v.invariant == R1 && v.detail == "block 0 does not read back";
    assert!(v.as_ref().is_some_and(|v| v.iter().any(lost)), "{v:?}");
}

// ---------------------------------------------------------------------------------------------
// Checked-in repros (§4.8).

fn checked_in(name: &str) -> Repro {
    let path = format!("{REGRESS}/{name}.repro");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let r = Repro::parse(&text).unwrap_or_else(|e| panic!("{path}: {e}"));
    assert_eq!(Repro::parse(&r.to_text()).expect("round trip"), r, "{path}");
    r
}

/// `r` with its planted bug removed.
fn unplanted(r: &Repro) -> Repro {
    let mut r = r.clone();
    r.config =
        r.config.split(' ').filter(|kv| !kv.starts_with("planted=")).collect::<Vec<_>>().join(" ");
    r
}

fn save(name: &str, what: &str, r: &Repro) {
    let m = minimize::<ChainSut>(r, 300);
    assert!(reproduces::<ChainSut>(&m.repro), "{name}");
    // Not vacuous: the minimised repro needs its bug.
    assert!(!reproduces::<ChainSut>(&unplanted(&m.repro)), "{name} reproduces unplanted");
    let text = format!("# {what}, minimised in {} replays.\n{}", m.replays, m.repro.to_text());
    std::fs::write(format!("{REGRESS}/{name}.repro"), text).expect("written");
}

#[test]
#[ignore = "rewrites tests/crash/regress"]
fn regenerate_planted_repros() {
    std::fs::create_dir_all(REGRESS).expect("the directory");
    for (bug, sc, expect) in witnessed() {
        let r = witness(bug, sc(), &expect);
        save(&bug.to_string(), &format!("CRASH planted bug {bug} (CH2 §6.4)"), &r);
    }
    for (bug, cfg, _) in refusal_witnessed() {
        let r = witness(bug, x_refused(cfg), &[R0]);
        save(
            &format!("{bug}-refused"),
            &format!("CRASH planted bug {bug} under DEC-196's refusal"),
            &r,
        );
    }
    let lost = lost_file_repro();
    let (_, t) = replay_trace::<ChainSut>(&lost).expect("a repro");
    assert_eq!(lost_file_lines(&t), LOST_FILE_LINES);
    let text =
        format!("# T1, the replay's block file lost (Core's open lines).\n{}", lost.to_text());
    std::fs::write(format!("{REGRESS}/T1-lost-file.repro"), text).expect("written");
}

#[test]
#[ignore = "rewrites tests/crash/regress"]
fn regenerate_connect_half_repros() {
    for (bug, sc, expect) in connect_halves() {
        let r = witness(bug, sc(), &expect);
        save(&format!("{bug}-core"), &format!("CRASH planted bug {bug}, core mode (CH3a a3)"), &r);
    }
}

#[test]
fn checked_in_connect_half_repros_replay() {
    for (bug, _, expect) in connect_halves() {
        let name = format!("{bug}-core");
        let r = checked_in(&name);
        assert_eq!(r.expect[0], expect[0], "{name}");
        assert!(r.config.contains("connect=core"), "{name}: {}", r.config);
        let got = replay::<ChainSut>(&r).expect("a repro");
        assert!(got.iter().any(|v| v.invariant == expect[0]), "{name}: {got:?}");
        assert!(!reproduces::<ChainSut>(&unplanted(&r)), "{name} reproduces unplanted");
    }
}

#[test]
fn checked_in_planted_repros_replay() {
    for (bug, _, expect) in witnessed() {
        let r = checked_in(&bug.to_string());
        assert_eq!(r.expect[0], expect[0], "{bug}");
        let got = replay::<ChainSut>(&r).expect("a repro");
        assert!(got.iter().any(|v| v.invariant == expect[0]), "{bug}: {got:?}");
        // `reproduces`, not `replay`: a bug that removes events can leave `crash_at` past the end.
        assert!(!reproduces::<ChainSut>(&unplanted(&r)), "{bug} reproduces unplanted");
    }
    for (bug, _, rule) in refusal_witnessed() {
        let name = format!("{bug}-refused");
        let r = checked_in(&name);
        let got = replay::<ChainSut>(&r).expect("a repro");
        assert!(refused_naming(&got, rule), "{name}: {got:?}");
        assert!(!reproduces::<ChainSut>(&unplanted(&r)), "{name} reproduces unplanted");
    }
}

/// `coins_replay_blind`'s smoke catches recovery's bugs: T7's lost marker by R7a on a nested
/// crash (idempotence), T8's wrong branch by R6 on a reorg window.
#[test]
fn coins_replay_blind_catches_recovery_bugs() {
    for (bug, expect) in [(Planted::T7, R7a), (Planted::T8, R6)] {
        let planted = |rng: &mut Seeded| with(coins_replay_blind(rng), bug);
        let r = sweep::<ChainSut>(BLIND_CASES, BLIND_SEED, &planted);
        let found: Vec<_> = r.findings.iter().map(|f| f.violation.invariant).collect();
        assert!(found.contains(&expect), "{bug}: {}", r.summary());
    }
}

// ---------------------------------------------------------------------------------------------
// b3: the subscriber bugs (§7.2's b3 cell), each on ST3a's witness scenario with b3's test
// subscriber, caught by the invariant ST3a's toy catches it with. Their repros are
// `tests/crash/regress/<bug>.repro`, rewritten by `regenerate_subscriber_repros`.

/// Every witnessed subscriber bug.
fn subscriber_witnessed() -> Vec<Witnessed> {
    use Planted::*;
    vec![
        (T10, || s1_sub(), vec![R11a]),
        (T16, || s11(), vec![R11b]),
        (T22, || power_loss(s1_sub()), vec![R11c]),
        (T28, || s13(), vec![R11b]),
        (T30, || s11r(), vec![FA1]),
        (T32, || s14(), vec![FA1]),
        (T33, || s11s(), vec![R11a, FA4]),
        (T35, || s11q(), vec![FA3]),
        // A lost gate rewind under W's flush at the fork leaves L above G on the image; its r1
        // witness on power-loss S11r (R11c) is kept as a replay (`t36_on_s11r_replays_r11c`).
        (T36, || power_loss(s2b(SubConfig::synced(0))), vec![D1b]),
        (T37, || s11d(2), vec![FA6]),
        (T38, || power_loss(s11d(2)), vec![R11c]),
        (T41, || s11d(1), vec![R11c]),
        // ST3a's T42: a backward B1 without the rewind gate. D45's open rewinds L at the boot,
        // so the image's L above the durable tip (D1b) is what finds it.
        (X19, || s2b(SubConfig::synced(0)), vec![D1b]),
        // S11c's control: a disabled index opens over pruned data.
        (NoPruneChk, || s11c(), vec![R11b]),
        // Sol r1's major 2: the record after the lock is released.
        (LateRecord, || s11c_w(), vec![R11b]),
        // Sol r2's blocker 1: the open returns with L off the durable tip's path.
        (NoOpenRwd, || s11o(), vec![R11a]),
    ]
}

fn subscriber_witness_of(bug: Planted) -> Repro {
    let (_, sc, expect) =
        subscriber_witnessed().into_iter().find(|(b, ..)| *b == bug).expect("a witnessed bug");
    witness(bug, sc(), &expect)
}

#[test]
fn t10_step5_before_b1() {
    subscriber_witness_of(Planted::T10);
}

#[test]
fn t16_lock_from_the_best_height() {
    subscriber_witness_of(Planted::T16);
}

#[test]
fn t22_locator_without_sync_wal() {
    subscriber_witness_of(Planted::T22);
}

#[test]
fn t28_lock_after_activation() {
    subscriber_witness_of(Planted::T28);
}

#[test]
fn t30_lock_at_the_locator_height() {
    subscriber_witness_of(Planted::T30);
}

#[test]
fn t32_dry_run_under_the_actual_locks() {
    subscriber_witness_of(Planted::T32);
}

#[test]
fn t33_uncapped_cadence_commit() {
    subscriber_witness_of(Planted::T33);
}

#[test]
fn t35_capture_by_best_height() {
    subscriber_witness_of(Planted::T35);
}

#[test]
fn t36_buffered_rewind_commit() {
    subscriber_witness_of(Planted::T36);
}

#[test]
fn t37_no_checkpoint_at_the_limit() {
    subscriber_witness_of(Planted::T37);
}

#[test]
fn t38_buffered_checkpoint() {
    subscriber_witness_of(Planted::T38);
}

#[test]
fn t41_checkpoint_without_the_keyed_rows() {
    subscriber_witness_of(Planted::T41);
}

#[test]
fn x19_backward_b1_without_the_gate() {
    subscriber_witness_of(Planted::X19);
}

#[test]
fn late_record_after_the_deregistration() {
    subscriber_witness_of(Planted::LateRecord);
}

#[test]
fn open_without_its_rewind_leaves_l_ahead_of_g() {
    subscriber_witness_of(Planted::NoOpenRwd);
}

#[test]
fn no_prune_check_opens_a_disabled_index_over_pruned_data() {
    subscriber_witness_of(Planted::NoPruneChk);
}

#[test]
#[ignore = "rewrites tests/crash/regress"]
fn regenerate_subscriber_repros() {
    std::fs::create_dir_all(REGRESS).expect("the directory");
    for (bug, sc, expect) in subscriber_witnessed() {
        let r = witness(bug, sc(), &expect);
        save(&bug.to_string(), &format!("CRASH planted subscriber bug {bug} (CH2 §7.2 b3)"), &r);
    }
}

/// T36's r1 witness, a power loss on S11r that loses the catch-up's buffered rewind commit and
/// leaves height rows above L (R11c), still replays; the witness search for T36 runs on S2b
/// (D45), so this one is kept as a checked-in replay.
#[test]
fn t36_on_s11r_replays_r11c() {
    let r = checked_in("T36-S11r");
    assert_eq!(r.expect[0], R11c);
    let got = replay::<ChainSut>(&r).expect("a repro");
    assert!(got.iter().any(|v| v.invariant == R11c), "{got:?}");
    assert!(!reproduces::<ChainSut>(&unplanted(&r)), "T36-S11r reproduces unplanted");
}

#[test]
fn checked_in_subscriber_repros_replay() {
    for (bug, _, expect) in subscriber_witnessed() {
        let r = checked_in(&bug.to_string());
        assert_eq!(r.expect[0], expect[0], "{bug}");
        let got = replay::<ChainSut>(&r).expect("a repro");
        assert!(got.iter().any(|v| v.invariant == expect[0]), "{bug}: {got:?}");
        assert!(!reproduces::<ChainSut>(&unplanted(&r)), "{bug} reproduces unplanted");
    }
}

#[test]
#[ignore = "a debugging aid: BUG=<bug> prints its witness search's findings"]
fn dump_subscriber_witness() {
    let bug: Planted = std::env::var("BUG").expect("BUG").parse().expect("a bug");
    let (_, sc, expect) =
        subscriber_witnessed().into_iter().find(|(b, ..)| *b == bug).expect("a witnessed bug");
    let r = explore(
        &with(sc(), bug),
        Mode::Witness {
            expect: expect.clone(),
            max_branches: 20_000,
        },
    );
    eprintln!("{}", r.summary());
    let mut seen = std::collections::BTreeSet::new();
    for f in r.findings.iter().filter(|f| seen.insert(format!("{:?}", f.violation.invariant))) {
        eprintln!("{:?}: {}\n{}", f.violation.invariant, f.violation.detail, f.repro.to_text());
    }
}

#[test]
#[ignore = "a debugging aid: REPRO=<path> replays a repro"]
fn dump_repro() {
    let text = std::fs::read_to_string(std::env::var("REPRO").expect("REPRO")).expect("a repro");
    let r = Repro::parse(&text).expect("a repro");
    // A forward-run violation (no crash, no fault): the forward run's trace.
    let (v, t) = if r.crash_at.is_none() && r.fault.is_none() {
        let cfg: ChainConfig = r.config.parse().expect("a config");
        let (t, v) =
            forward_trace::<ChainSut>(&Scenario::new("repro", cfg, ops(&r.workload.join(";"))));
        (v, t)
    } else {
        replay_trace::<ChainSut>(&r).expect("replays")
    };
    eprintln!("{v:?}");
    // TRACE=<substring>: the trace entries whose debug form contains it.
    if let Ok(pat) = std::env::var("TRACE") {
        for e in t.entries.iter().map(|e| format!("{e:?}")).filter(|e| e.contains(&pat)) {
            eprintln!("{}", &e[..e.len().min(240)]);
        }
    }
}
