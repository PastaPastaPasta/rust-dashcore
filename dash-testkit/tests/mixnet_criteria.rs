//! O12b-2a's judge: the judged range, reorgs, provenance, chaos, C1, C2 and C6, and both gates.

use std::collections::BTreeMap;
use std::ops::RangeInclusive;

use dash_testkit::mixnet::contract::{
    Binaries, Binary, Binding, Block, Caps, Chaos, Clock, Commitment, Context, DigestCompare,
    DigestResult, Envelope, Hex32, Host, Impl, LlmqParams, Member, MnDiff, Preset, Record, Reorg,
    RolePlan, Run, RunKind, SlotId, V1, Watermark,
};
use dash_testkit::mixnet::criteria::{
    Config, CriterionReport, Gate, REHEARSAL_BANNER, Report, Verdict, judge,
};
use serde_json::json;

const H0: u32 = 100;
const HN: u32 = 150;

fn h(byte: u8) -> Hex32 {
    format!("{byte:02x}").repeat(32).parse().unwrap()
}

fn hash(tag: u8, height: u32) -> Hex32 {
    format!("{:0>64}", format!("{tag:02x}{height:08x}")).parse().unwrap()
}

fn pro(slot: SlotId) -> Hex32 {
    match slot {
        SlotId::Regular(i) => h(0x30 + i as u8),
        SlotId::Evo(i) => h(0x50 + i as u8),
        SlotId::Node(i) => h(0x70 + i as u8),
    }
}

fn reg(i: u16) -> SlotId {
    SlotId::Regular(i)
}

fn params(llmq_type: u8, rotation: bool, evo_only: bool) -> LlmqParams {
    let (mining_window_start, mining_window_end) = if rotation {
        (12, 20)
    } else {
        (10, 18)
    };
    let (size, min_size) = if rotation {
        (4, 4)
    } else {
        (3, 2)
    };
    LlmqParams {
        llmq_type,
        name: format!("llmq_{llmq_type}"),
        size,
        min_size,
        dkg_interval: 24,
        mining_window_start,
        mining_window_end,
        rotation,
        signing_active_quorum_count: 2,
        evo_only,
    }
}

fn row(slot: SlotId, height: u32, penalty: Option<u32>, ban: Option<i32>) -> Record {
    Record::MnDiff(MnDiff {
        height,
        pro_tx: pro(slot),
        pose_penalty: penalty,
        pose_ban_height: ban,
        pose_revived_height: None,
    })
}

/// A row with all three PoSe fields.
fn pose(pro_tx: Hex32, height: u32, penalty: u32, ban: i32) -> Record {
    Record::MnDiff(MnDiff {
        height,
        pro_tx,
        pose_penalty: Some(penalty),
        pose_ban_height: Some(ban),
        pose_revived_height: Some(-1),
    })
}

fn baseline(slot: SlotId, height: u32) -> Record {
    pose(pro(slot), height, 0, -1)
}

fn diffs_of(slot: SlotId) -> impl Fn(&Record) -> bool {
    move |r| matches!(r, Record::MnDiff(d) if d.pro_tx == pro(slot))
}

fn digests_of(slot: SlotId) -> impl Fn(&Record) -> bool {
    move |r| matches!(r, Record::DigestCompare(d) if d.node == slot)
}

fn watermark(height: u32) -> impl Fn(&Record) -> bool {
    move |r| matches!(r, Record::Watermark(w) if w.height == height)
}

/// A commitment for the quorum based at `base` on chain 0.
fn commitment(
    llmq_type: u8,
    index: u16,
    base: u32,
    mined: u32,
    null: bool,
    members: &[(u16, bool)],
) -> Record {
    Record::Commitment(Commitment {
        llmq_type,
        quorum_hash: hash(0, base),
        index,
        base_height: base,
        mined_height: mined,
        null,
        members: members
            .iter()
            .map(|&(i, valid)| Member {
                pro_tx: pro(reg(i)),
                valid,
            })
            .collect(),
        vvec_hash: hash(0x41, base),
    })
}

fn digest(slot: SlotId, from: u32, to: u32, result: DigestResult) -> Record {
    Record::DigestCompare(DigestCompare {
        node: slot,
        from_height: from,
        to_height: to,
        result,
    })
}

fn chaos(id: &str, target: SlotId, start_seq: u64, end_seq: u64) -> Record {
    Record::Chaos(Chaos {
        id: id.into(),
        action: "stop".into(),
        targets: vec![target],
        start_seq,
        end_seq,
    })
}

/// Records in seq order; record `i` has seq `i + 1`.
#[derive(Clone)]
struct Log {
    records: Vec<Record>,
    plan: RolePlan,
    block_seq: BTreeMap<u32, u64>,
}

impl Log {
    /// An alt5 `Run`, its `LlmqParams`, a binding and a PoSe baseline per masternode slot, and
    /// the first watermark at `H0`.
    fn start(caps: &str, kind: RunKind, types: &[LlmqParams]) -> Self {
        let caps: Caps = caps.parse().unwrap();
        let plan = RolePlan::new(Preset::Alt5, caps.clone(), Context::Regtest).unwrap();
        let mut log = Log {
            records: Vec::new(),
            plan: plan.clone(),
            block_seq: BTreeMap::new(),
        };
        let binary = |b| Binary {
            sha256: h(b),
            version: "v".into(),
        };
        log.push(Record::Run(Run {
            run_kind: kind,
            context: Context::Regtest,
            preset: Preset::Alt5,
            caps,
            host: Host {
                nproc: 8,
                mem: 1 << 34,
                disk: 1 << 38,
            },
            clock: Clock {
                source: "chrony".into(),
                offset_ms: 0,
            },
            binaries: Binaries {
                dashd: binary(1),
                rdashd: Some(binary(2)),
            },
        }));
        for p in types {
            log.push(Record::LlmqParams(p.clone()));
        }
        let masternodes: Vec<_> = plan.slots.iter().filter(|s| s.slot.is_masternode()).collect();
        for s in &masternodes {
            log.push(Record::Binding(Binding {
                slot: s.slot,
                pro_tx: pro(s.slot),
                label: s.label,
                runs: s.runs,
                exe_sha256: h(if s.runs == Impl::Rust {
                    2
                } else {
                    1
                }),
                from_height: 0,
                from_seq: 0,
            }));
        }
        log.push(Record::Watermark(Watermark {
            height: H0,
        }));
        for s in &masternodes {
            log.push(baseline(s.slot, H0));
        }
        log
    }

    /// `start`, chain 0 over the judged range, then `finish`.
    fn run(caps: &str, kind: RunKind, types: &[LlmqParams]) -> Self {
        let mut log = Log::start(caps, kind, types);
        log.blocks(0, H0 + 1..=HN, 0);
        log.finish();
        log
    }

    /// A finished alt5 Stage-A run without its one in-range llmq_test commitment.
    fn no_commitment() -> Self {
        Log::run("mn,dkg", RunKind::StageA, &[params(100, false, false)])
    }

    /// A clean alt5 Stage-A run: one opportunity (type 100 base 120, window 130-138), whose
    /// commitment has Rust members reg0 and reg2.
    fn clean() -> Self {
        Log::no_commitment().with(commitment(
            100,
            0,
            120,
            131,
            false,
            &[(0, true), (1, true), (2, true)],
        ))
    }

    fn push(&mut self, record: Record) {
        if let Record::Block(b) = &record {
            self.block_seq.insert(b.height, self.records.len() as u64 + 1);
        }
        self.records.push(record);
    }

    /// Blocks `heights` on chain `tag`; the first links to chain `below`.
    fn blocks(&mut self, tag: u8, heights: RangeInclusive<u32>, below: u8) {
        let first = *heights.start();
        for height in heights {
            self.push(Record::Block(Block {
                height,
                hash: hash(tag, height),
                prev: hash(
                    if height == first {
                        below
                    } else {
                        tag
                    },
                    height - 1,
                ),
                miner: None,
            }));
        }
    }

    /// The last watermark at `HN` and an equal digest compare for every node but node0.
    fn finish(&mut self) {
        self.push(Record::Watermark(Watermark {
            height: HN,
        }));
        let nodes: Vec<SlotId> =
            self.plan.slots.iter().map(|s| s.slot).filter(|&s| s != SlotId::CONTROLLER).collect();
        for slot in nodes {
            self.push(digest(slot, H0 + 1, HN - 6, DigestResult::Equal));
        }
    }

    fn with(mut self, record: Record) -> Self {
        self.push(record);
        self
    }

    fn without(mut self, drop: impl Fn(&Record) -> bool) -> Self {
        self.records.retain(|r| !drop(r));
        self
    }

    fn edit_bindings(mut self, f: impl Fn(&mut Binding)) -> Self {
        for r in &mut self.records {
            if let Record::Binding(b) = r {
                f(b);
            }
        }
        self
    }

    fn envelopes(&self) -> Vec<Envelope> {
        self.records
            .iter()
            .enumerate()
            .map(|(i, record)| Envelope {
                v: V1,
                run: "r".into(),
                seq: i as u64 + 1,
                mono_ns: i as u64,
                src: "watch".into(),
                record: record.clone(),
            })
            .collect()
    }

    fn judge_with(&self, gate: Gate, config: &Config) -> Report {
        judge(&self.envelopes(), gate, config)
    }

    fn judge(&self) -> Report {
        self.judge_with(Gate::StageA, &Config::default())
    }

    fn rehearsal(&self) -> bool {
        self.judge_with(Gate::Rehearsal, &Config::default()).passed
    }

    fn c(&self, n: usize) -> CriterionReport {
        self.judge().criteria[n - 1].clone()
    }

    fn verdict(&self, n: usize) -> Verdict {
        self.c(n).verdict
    }
}

fn fail(witness: &str) -> Verdict {
    Verdict::Fail(witness.into())
}

fn incomplete(missing: &str) -> Verdict {
    Verdict::Incomplete(missing.into())
}

fn vacuous(reason: &str) -> Verdict {
    Verdict::Vacuous(reason.into())
}

fn verdicts(report: &Report) -> Vec<Verdict> {
    report.criteria.iter().map(|c| c.verdict.clone()).collect()
}

fn later() -> Verdict {
    Verdict::NotRun("O12b-2c".into())
}

/// C1, C2 and C6 pass; C3-C5 wait for O12b-2c.
fn clean_verdicts() -> [Verdict; 6] {
    [Verdict::Pass, Verdict::Pass, later(), later(), later(), Verdict::Pass]
}

#[test]
fn a_clean_run_passes_c1_c2_and_c6_but_not_the_stage_a_gate() {
    let log = Log::clean();
    let report = log.judge();
    assert_eq!(verdicts(&report), clean_verdicts());
    let counts: Vec<_> = report.criteria.iter().map(|c| (c.n_applicable, c.n_checked)).collect();
    assert_eq!(counts, [(2, 2), (3, 3), (0, 0), (0, 0), (0, 0), (5, 5)]);
    assert!(report.criteria.iter().all(|c| c.exemptions.is_empty()));
    assert_eq!((report.gate, report.passed, report.banner), (Gate::StageA, false, None));
    let rehearsal = log.judge_with(Gate::Rehearsal, &Config::default());
    assert_eq!((rehearsal.passed, rehearsal.banner), (true, Some(REHEARSAL_BANNER)));
    assert_eq!(rehearsal.criteria, report.criteria);
    let v = serde_json::to_value(&report).unwrap();
    assert_eq!(v["gate"], "stage-a");
    assert_eq!(v["criteria"][0]["criterion"], "C1");
    assert_eq!(v["criteria"][0]["verdict"], json!({"status": "pass"}));
    assert_eq!(v["criteria"][2]["verdict"], json!({"status": "not-run", "detail": "O12b-2c"}));
}

#[test]
fn c1_needs_a_final_commitment_with_valid_rust_members() {
    let at = "type 100 base 120";
    let mined = |members: &[(u16, bool)]| {
        Log::no_commitment().with(commitment(100, 0, 120, 131, false, members))
    };
    let reg2 = format!("{at}: {} (slot reg2) invalid", pro(reg(2)));
    assert_eq!(mined(&[(0, true), (2, false)]).verdict(1), fail(&reg2));
    let dashd_invalid = mined(&[(0, true), (1, false), (3, false)]).c(1);
    assert_eq!((dashd_invalid.verdict, dashd_invalid.n_applicable), (Verdict::Pass, 1));
    assert_eq!(mined(&[(1, true), (3, true)]).verdict(1), vacuous("no applicable instance"));
    let null = Log::no_commitment().with(commitment(100, 0, 120, 130, true, &[]));
    assert_eq!(null.verdict(1), fail(&format!("{at}: null")));
    assert_eq!((null.c(1).n_applicable, null.c(1).n_checked), (1, 1));
    let late = null.clone().with(commitment(100, 0, 120, 132, false, &[(0, true)]));
    assert_eq!(late.verdict(1), Verdict::Pass);
    let none = format!("{at}: no commitment");
    assert_eq!(Log::no_commitment().verdict(1), incomplete(&none));
    for (index, base, mined) in [(0, 120, 129), (0, 120, 139), (0, 121, 131), (1, 120, 131)] {
        let off =
            Log::no_commitment().with(commitment(100, index, base, mined, false, &[(0, true)]));
        assert_eq!(off.verdict(1), incomplete(&none), "{index} {base} {mined}");
    }
    let other = Log::no_commitment().with(commitment(104, 0, 120, 131, false, &[(0, true)]));
    assert_eq!(other.verdict(1), incomplete(&none));
    let mut elsewhere = commitment(100, 0, 120, 131, false, &[(0, true)]);
    if let Record::Commitment(c) = &mut elsewhere {
        c.quorum_hash = hash(1, 120);
    }
    assert_eq!(Log::no_commitment().with(elsewhere).verdict(1), incomplete(&none));
}

#[test]
fn c1_counts_every_rotation_index_and_reads_only_the_runs_params() {
    let types = [params(100, false, false), params(103, true, false)];
    let both = &[(0, true), (2, true)];
    let log = Log::run("mn,dkg", RunKind::StageA, &types)
        .with(commitment(100, 0, 120, 131, false, both))
        .with(commitment(103, 0, 120, 133, false, both));
    assert_eq!(log.verdict(1), incomplete("type 103 base 121: no commitment"));
    let log = log.with(commitment(103, 1, 121, 140, false, both));
    assert_eq!((log.verdict(1), log.c(1).n_checked), (Verdict::Pass, 6));

    let evo_only = Log::run("mn,dkg", RunKind::StageA, &[params(106, false, true)]);
    assert_eq!(evo_only.verdict(1), vacuous("no applicable instance"));
    let none = Log::run("mn,dkg", RunKind::StageA, &[]);
    assert_eq!(none.verdict(1), incomplete("no LlmqParams record"));
    let late = Log::clean().with(Record::LlmqParams(params(104, false, false)));
    assert_eq!(late.verdict(1), incomplete("LlmqParams type 104 after a Watermark"));
    let twice = Log::run(
        "mn,dkg",
        RunKind::StageA,
        &[params(100, false, false), params(100, false, false)],
    );
    assert_eq!(twice.verdict(1), incomplete("LlmqParams type 100 twice"));
    let unusable: [fn(&mut LlmqParams); 5] = [
        |p| p.dkg_interval = 0,
        |p| p.mining_window_end = 24,
        |p| p.mining_window_start = 21,
        |p| p.signing_active_quorum_count = 0,
        |p| p.signing_active_quorum_count = 13,
    ];
    for (i, edit) in unusable.into_iter().enumerate() {
        let mut p = params(103, true, false);
        edit(&mut p);
        let log = Log::run("mn,dkg", RunKind::StageA, &[p]);
        assert_eq!(log.verdict(1), incomplete("LlmqParams type 103 has no DKG window"), "{i}");
        assert_eq!(log.verdict(2), Verdict::Pass, "{i}");
    }
}

#[test]
fn c2_fails_a_penalty_rise_on_a_rust_bound_protx() {
    let rose =
        |i, height, p| format!("{} (slot reg{i}) at {height}: penalty rose to {p}", pro(reg(i)));
    let rise = Log::clean().with(row(reg(2), 135, Some(66), None));
    assert_eq!(rise.verdict(2), fail(&rose(2, 135, 66)));
    let first =
        rise.clone().with(row(reg(0), 140, Some(66), None)).with(row(reg(0), 141, Some(65), None));
    assert_eq!(first.verdict(2), fail(&rose(2, 135, 66)));
    let falling = Log::clean()
        .without(diffs_of(reg(2)))
        .with(pose(pro(reg(2)), H0, 10, -1))
        .with(row(reg(2), 101, Some(9), None))
        .with(row(reg(2), 102, Some(8), None));
    assert_eq!(falling.verdict(2), Verdict::Pass);
    let up = falling.with(row(reg(2), 103, Some(9), None));
    assert_eq!(up.verdict(2), fail(&rose(2, 103, 9)));
    let punished = Log::clean().with(row(reg(4), 140, Some(66), Some(140)));
    assert_eq!(punished.verdict(2), fail(&rose(4, 140, 66)));
    let operator_change = Log::clean().with(row(reg(4), 140, None, Some(140)));
    assert_eq!(operator_change.verdict(2), Verdict::Pass);
    assert_eq!(Log::clean().with(row(reg(1), 135, Some(66), Some(135))).verdict(2), Verdict::Pass);
    let before = Log::clean().with(row(reg(2), 90, Some(66), None)).with(baseline(reg(2), 80));
    assert_eq!(before.verdict(2), Verdict::Pass);

    let missing = incomplete(&format!("no PoSe baseline for {}", pro(reg(2))));
    let unbased = Log::clean().without(diffs_of(reg(2)));
    assert_eq!(unbased.verdict(2), missing);
    assert_eq!(unbased.clone().with(row(reg(2), H0, Some(0), None)).verdict(2), missing);
    assert_eq!(unbased.clone().with(baseline(reg(2), H0 + 1)).verdict(2), missing);
    let hidden = unbased.clone().with(pose(pro(reg(2)), H0 + 1, 66, -1));
    assert_eq!(hidden.verdict(2), missing);
    assert_eq!(unbased.clone().with(baseline(reg(2), H0)).verdict(2), Verdict::Pass);
    let dashd = Log::clean().without(diffs_of(reg(1))).c(2);
    assert_eq!((dashd.verdict, dashd.n_applicable), (Verdict::Pass, 3));
}

#[test]
fn c2_follows_a_slot_to_its_new_protx() {
    let moved = Log::clean().with(Record::Binding(Binding {
        slot: reg(2),
        pro_tx: h(0x3f),
        label: Impl::Rust,
        runs: Impl::Rust,
        exe_sha256: h(2),
        from_height: 130,
        from_seq: 1,
    }));
    let new_mn = format!("no PoSe baseline for {}", h(0x3f));
    assert_eq!(moved.verdict(2), incomplete(&new_mn));
    let moved = moved.with(pose(h(0x3f), 130, 0, -1));
    assert_eq!((moved.verdict(2), moved.c(2).n_applicable), (Verdict::Pass, 4));
    let old = pro(reg(2));
    assert_eq!(moved.clone().with(row(reg(2), 140, Some(66), None)).verdict(2), Verdict::Pass);
    let before_move = moved.clone().with(row(reg(2), 125, Some(66), None));
    assert_eq!(
        before_move.verdict(2),
        fail(&format!("{old} (slot reg2) at 125: penalty rose to 66"))
    );
    let new_rise = moved.clone().with(pose(h(0x3f), 140, 66, -1));
    assert_eq!(
        new_rise.verdict(2),
        fail(&format!("{} (slot reg2) at 140: penalty rose to 66", h(0x3f)))
    );
    let member = |pro_tx: Hex32| {
        let mut c = commitment(100, 0, 120, 131, false, &[(0, true)]);
        if let Record::Commitment(c) = &mut c {
            c.members.push(Member {
                pro_tx,
                valid: false,
            });
        }
        moved.clone().without(|r| matches!(r, Record::Commitment(_))).with(c)
    };
    let late_bind = Log::clean().without(|r| matches!(r, Record::Commitment(_))).with(
        Record::Binding(Binding {
            slot: reg(2),
            pro_tx: h(0x3f),
            label: Impl::Rust,
            runs: Impl::Rust,
            exe_sha256: h(2),
            from_height: 110,
            from_seq: 1,
        }),
    );
    assert!(matches!(member(old.clone()).verdict(1), Verdict::Fail(_)));
    let rebound = late_bind.with(pose(h(0x3f), 110, 0, -1)).with(commitment(
        100,
        0,
        120,
        131,
        false,
        &[(0, true), (2, false)],
    ));
    assert_eq!(rebound.verdict(1), Verdict::Pass, "reg2's old proTx is not reg2's at base 120");
}

#[test]
fn every_rust_masternode_slot_needs_a_binding() {
    let unbound = Log::clean().without(|r| matches!(r, Record::Binding(b) if b.slot == reg(2)));
    let why = incomplete("no binding for reg2 at 101");
    assert_eq!(verdicts(&unbound.judge())[..2], [why.clone(), why]);
    assert_eq!(unbound.verdict(6), Verdict::Pass);
    let late = Log::clean().edit_bindings(|b| {
        if b.slot == reg(2) {
            b.from_height = H0 + 2;
        }
    });
    assert_eq!(late.verdict(1), incomplete("no binding for reg2 at 101"));
    let dashd = Log::clean().without(|r| matches!(r, Record::Binding(b) if b.slot == reg(1)));
    assert_eq!(verdicts(&dashd.judge()), clean_verdicts());
}

#[test]
fn c6_compares_every_node_with_node0() {
    let set = |slot, from, to, result| {
        Log::clean().without(digests_of(slot)).with(digest(slot, from, to, result))
    };
    let mismatch = |height| DigestResult::Mismatch {
        height,
    };
    assert_eq!(
        set(reg(3), H0 + 1, HN - 6, mismatch(120)).verdict(6),
        fail("reg3 differs from node0 at 120")
    );
    let two = set(reg(3), H0 + 1, HN - 6, mismatch(130)).without(digests_of(reg(1))).with(digest(
        reg(1),
        H0 + 1,
        HN - 6,
        mismatch(125),
    ));
    assert_eq!(two.verdict(6), fail("reg1 differs from node0 at 125"));
    assert_eq!(
        set(reg(3), H0 + 1, 110, mismatch(105)).verdict(6),
        fail("reg3 differs from node0 at 105")
    );
    let retried = set(reg(3), H0 + 1, HN - 6, mismatch(120)).with(digest(
        reg(3),
        H0 + 1,
        HN - 6,
        DigestResult::Equal,
    ));
    assert_eq!(retried.verdict(6), fail("reg3 differs from node0 at 120"));
    let again = Log::clean().with(digest(reg(3), H0 + 1, HN - 6, DigestResult::Equal));
    assert_eq!(again.verdict(6), incomplete("reg3 has 2 digest compares"));
    let not_run = || DigestResult::NotRun {
        reason: "no sink".into(),
    };
    let unrun = set(reg(1), H0 + 1, HN - 6, not_run());
    assert_eq!(
        (unrun.verdict(6), unrun.c(6).n_checked),
        (Verdict::NotRun("reg1: no sink".into()), 4)
    );
    let stopped = || DigestResult::Incomplete {
        reason: "extract stopped".into(),
    };
    assert_eq!(
        set(reg(1), H0 + 1, HN - 6, stopped()).verdict(6),
        incomplete("reg1: extract stopped")
    );
    let both = set(reg(1), H0 + 1, HN - 6, not_run()).without(digests_of(reg(4))).with(digest(
        reg(4),
        H0 + 1,
        HN - 6,
        stopped(),
    ));
    assert_eq!(both.verdict(6), incomplete("reg4: extract stopped"));
    assert_eq!(
        set(reg(0), H0 + 1, 140, DigestResult::Equal).verdict(6),
        incomplete("reg0 digest covers [101, 140], not [101, 144]")
    );
    assert_eq!(
        set(reg(0), H0 + 2, HN, DigestResult::Equal).verdict(6),
        incomplete("reg0 digest covers [102, 150], not [101, 144]")
    );
    assert_eq!(
        Log::clean().without(digests_of(reg(4))).verdict(6),
        incomplete("no digest compare for reg4")
    );
    let config = |catchup| Config {
        catchup,
        ..Config::default()
    };
    let strict = Log::clean().judge_with(Gate::StageA, &config(0));
    assert_eq!(
        strict.criteria[5].verdict,
        incomplete("reg0 digest covers [101, 144], not [101, 150]")
    );
    let wide = Log::clean().judge_with(Gate::StageA, &config(60));
    assert_eq!(
        wide.criteria[5].verdict,
        incomplete("the judged range ends within the 60 catch-up")
    );
}

#[test]
fn the_judged_range_must_be_whole_and_linked() {
    let every = |log: &Log, why: &str| {
        assert_eq!(verdicts(&log.judge()), vec![incomplete(why); 6], "{why}");
        assert!(!log.rehearsal(), "{why}");
    };
    every(&Log::clean().without(watermark(HN)), "no judged range");
    every(&Log::clean().without(|r| matches!(r, Record::Watermark(_))), "no judged range");
    every(
        &Log::clean().with(Record::Watermark(Watermark {
            height: H0,
        })),
        "no judged range",
    );
    every(
        &Log::clean().without(|r| matches!(r, Record::Block(b) if b.height == 125)),
        "height 125 missing",
    );
    let mut broken = Log::clean();
    for r in &mut broken.records {
        if let Record::Block(b) = r
            && b.height == 126
        {
            b.prev = hash(9, 125);
        }
    }
    every(&broken, "height 126 does not link to height 125");
    let fork = Log::clean().with(Record::Block(Block {
        height: 130,
        hash: hash(7, 130),
        prev: hash(0, 129),
        miner: None,
    }));
    every(&fork, "height 130 has two blocks");
    let mut unlinked_first = Log::start("mn,dkg", RunKind::StageA, &[params(100, false, false)]);
    unlinked_first.blocks(0, H0 + 1..=HN, 9);
    unlinked_first.finish();
    assert!(unlinked_first.rehearsal());
}

#[test]
fn provenance_decides_before_any_criterion() {
    let rehearsal = Log::run("mn,dkg", RunKind::Rehearsal, &[params(100, false, false)]);
    assert_eq!(verdicts(&rehearsal.judge()), vec![vacuous("rehearsal"); 6]);
    assert!(!rehearsal.judge().passed);
    assert!(rehearsal.rehearsal());
    let unfinished = rehearsal.clone().without(watermark(HN));
    assert_eq!(verdicts(&unfinished.judge()), vec![vacuous("rehearsal"); 6]);
    assert!(!unfinished.rehearsal());

    let twin = Log::run("mn", RunKind::StageA, &[params(100, false, false)]).with(commitment(
        100,
        0,
        120,
        131,
        true,
        &[],
    ));
    let core_twin = vacuous("CoreTwin");
    assert_eq!(
        verdicts(&twin.judge()),
        [core_twin.clone(), core_twin.clone(), later(), core_twin, later(), Verdict::Pass]
    );

    let rebind = |runs: Impl, exe: u8, slot: SlotId| {
        Log::clean().edit_bindings(|b| {
            if b.slot == slot {
                (b.runs, b.label, b.exe_sha256) = (runs, runs, h(exe));
            }
        })
    };
    let rust_exe = incomplete("binding exe is not the run's rdashd");
    assert_eq!(
        verdicts(&rebind(Impl::Rust, 1, reg(2)).judge()),
        [rust_exe.clone(), rust_exe, later(), later(), later(), Verdict::Pass]
    );
    assert_eq!(
        rebind(Impl::Dashd, 2, reg(1)).verdict(1),
        incomplete("binding exe is not the run's dashd")
    );
    let role = format!("binding {} for reg1 is not the plan's role", pro(reg(1)));
    assert_eq!(rebind(Impl::Rust, 2, reg(1)).verdict(2), incomplete(&role));

    let mut lines = Log::clean().envelopes();
    lines[3].run = "other".into();
    let report = judge(&lines, Gate::Rehearsal, &Config::default());
    assert_eq!(verdicts(&report), vec![incomplete("record 3 belongs to another run"); 6]);
    assert!(!report.passed);
    let report = judge(&[], Gate::StageA, &Config::default());
    assert_eq!(verdicts(&report), vec![incomplete("the log has no records"); 6]);
}

/// Blocks 101-135 on chain 0 with `before` mined at 131, a reorg to fork height 129, then blocks
/// 130-150 on chain 1 with `after` mined at 133. Also returns the seq of chain 0's block 135.
fn reorged(before: Record, after: Record) -> (Log, u64) {
    let mut log = Log::start("mn,dkg", RunKind::StageA, &[params(100, false, false)]);
    log.blocks(0, H0 + 1..=135, 0);
    let stale = log.block_seq[&135];
    log.push(before);
    log.push(row(reg(2), 132, Some(66), None));
    log.push(Record::Reorg(Reorg {
        fork_height: 129,
        old_tip: hash(0, 135),
        new_tip: hash(1, HN),
    }));
    log.blocks(1, 130..=HN, 0);
    log.push(after);
    log.finish();
    (log, stale)
}

#[test]
fn a_reorg_retracts_what_it_replaced() {
    let null = |mined| commitment(100, 0, 120, mined, true, &[]);
    let fine = |mined| commitment(100, 0, 120, mined, false, &[(0, true), (2, true)]);
    let (healed, stale) = reorged(null(131), fine(133));
    assert_eq!(verdicts(&healed.judge()), clean_verdicts());
    assert!(healed.rehearsal());
    assert_eq!(reorged(fine(131), null(133)).0.verdict(1), fail("type 100 base 120: null"));
    let late = healed.clone().with(row(reg(2), 140, Some(66), None));
    assert!(matches!(late.verdict(2), Verdict::Fail(w) if w.contains("at 140")));

    let no_allow = Config {
        chaos_allow: Some(0),
        ..Config::default()
    };
    let at_fork = healed.clone().with(row(reg(2), 129, Some(66), None));
    assert!(matches!(at_fork.verdict(2), Verdict::Fail(_)));
    let report = at_fork.with(chaos("c", reg(2), stale, stale)).judge_with(Gate::StageA, &no_allow);
    assert_eq!(report.criteria[1].verdict, Verdict::Pass, "a retracted block's seq maps to 129");
    assert_eq!(report.criteria[1].exemptions.len(), 1);
}

#[test]
fn chaos_exempts_c1_and_c2_but_never_c6() {
    let log = Log::no_commitment()
        .with(commitment(100, 0, 120, 131, false, &[(0, true), (2, false)]))
        .with(row(reg(2), 135, Some(66), None))
        .with(digest(
            reg(2),
            H0 + 1,
            HN - 6,
            DigestResult::Mismatch {
                height: 135,
            },
        ));
    let seq = |height| log.block_seq[&height];
    let hit = log.clone().with(chaos("stop-1", reg(2), seq(128), seq(130)));
    let report = hit.judge();
    let (c1, c2) = (&report.criteria[0], &report.criteria[1]);
    assert_eq!((&c1.verdict, c1.n_applicable, c1.n_checked), (&Verdict::Pass, 2, 1));
    assert_eq!(
        c1.exemptions,
        [format!("type 100 base 120: {} (slot reg2): chaos stop-1", pro(reg(2)))]
    );
    assert_eq!((&c2.verdict, c2.n_applicable, c2.n_checked), (&Verdict::Pass, 3, 3));
    assert_eq!(c2.exemptions, [format!("{} (slot reg2) at 135: chaos stop-1", pro(reg(2)))]);
    assert_eq!(report.criteria[5].verdict, fail("reg2 differs from node0 at 135"));
    assert!(report.criteria[5].exemptions.is_empty());

    let elsewhere = log.clone().with(chaos("stop-1", reg(0), seq(128), seq(130)));
    assert!(matches!(elsewhere.verdict(1), Verdict::Fail(_)));
    assert!(matches!(elsewhere.verdict(2), Verdict::Fail(_)));
    let early = log.clone().with(chaos("stop-1", reg(2), seq(101), seq(102)));
    let allow = |allow| Config {
        chaos_allow: Some(allow),
        ..Config::default()
    };
    let report = early.judge_with(Gate::StageA, &allow(0));
    assert!(matches!(report.criteria[0].verdict, Verdict::Fail(_)));
    assert!(matches!(report.criteria[1].verdict, Verdict::Fail(_)));
    let report = early.judge();
    assert_eq!(report.criteria[0].verdict, Verdict::Pass, "the default allow is one interval");
    assert!(matches!(report.criteria[1].verdict, Verdict::Fail(_)));
    for (from, to, exempt) in
        [(110, 120, true), (110, 119, false), (138, 140, true), (139, 140, false)]
    {
        let report = log
            .clone()
            .with(chaos("edge", reg(2), seq(from), seq(to)))
            .judge_with(Gate::StageA, &allow(0));
        assert_eq!(report.criteria[0].exemptions.len(), usize::from(exempt), "{from}-{to}");
    }

    let lone = Log::no_commitment().with(commitment(100, 0, 120, 131, false, &[(2, false)]));
    let lone =
        lone.clone().with(chaos("stop-1", reg(2), lone.block_seq[&128], lone.block_seq[&130]));
    assert_eq!(lone.verdict(1), vacuous("every applicable instance is exempt"));

    let last = log.records.len() as u64;
    for (start, end) in [(seq(130), seq(128)), (seq(128), last + 2)] {
        let bad = log.clone().with(chaos("bad", reg(2), start, end));
        let why = incomplete("chaos bad is not a window of the log");
        assert_eq!(verdicts(&bad.judge())[..2], [why.clone(), why]);
    }
}

#[test]
fn a_rotation_index_is_exempt_only_inside_its_cycles_window() {
    let types = [params(100, false, false), params(103, true, false)];
    let log = Log::run("mn,dkg", RunKind::StageA, &types)
        .with(commitment(100, 0, 120, 131, false, &[(0, true)]))
        .with(commitment(103, 0, 120, 133, false, &[(0, true)]))
        .with(commitment(103, 1, 121, 135, false, &[(2, false)]));
    let at = |height| log.block_seq[&height];
    let no_allow = Config {
        chaos_allow: Some(0),
        ..Config::default()
    };
    let judged = |height| {
        log.clone()
            .with(chaos("c", reg(2), at(height), at(height)))
            .judge_with(Gate::StageA, &no_allow)
            .criteria[0]
            .verdict
            .clone()
    };
    assert_eq!(judged(140), Verdict::Pass);
    assert!(matches!(judged(141), Verdict::Fail(w) if w.contains("type 103 base 121")));

    let mut broken = params(100, false, false);
    broken.dkg_interval = 0;
    let unusable = Log::run("mn,dkg", RunKind::StageA, &[broken]);
    let with_chaos = unusable.clone().with(chaos(
        "c",
        reg(2),
        unusable.block_seq[&130],
        unusable.block_seq[&131],
    ));
    assert_eq!(with_chaos.verdict(2), incomplete("LlmqParams type 100 has no DKG window"));
    assert_eq!(with_chaos.judge_with(Gate::StageA, &no_allow).criteria[1].verdict, Verdict::Pass);
}

#[test]
fn a_null_commitment_is_exempt_only_when_chaos_stops_enough_members() {
    let stop = |log: &Log, targets: &[SlotId]| {
        let mut log = log.clone();
        for (i, &slot) in targets.iter().enumerate() {
            let record = chaos(&format!("c{i}"), slot, log.block_seq[&128], log.block_seq[&130]);
            log.push(record);
        }
        log
    };
    let null = Log::no_commitment().with(commitment(100, 0, 120, 131, true, &[]));
    let one = stop(&null, &[reg(2)]).c(1);
    assert_eq!((one.verdict, one.n_checked), (fail("type 100 base 120: null"), 1));
    let two = stop(&null, &[reg(2), reg(1)]).c(1);
    assert_eq!(two.verdict, vacuous("every applicable instance is exempt"));
    assert_eq!(two.exemptions, ["type 100 base 120: null: chaos on reg1, reg2"]);
    let elsewhere = stop(&null, &[reg(1), SlotId::CONTROLLER]).c(1);
    assert_eq!(elsewhere.verdict, fail("type 100 base 120: null"));

    let types = [params(100, false, false), params(103, true, false)];
    let rotated = Log::run("mn,dkg", RunKind::StageA, &types)
        .with(commitment(100, 0, 120, 131, false, &[(0, true)]))
        .with(commitment(103, 0, 120, 133, false, &[(0, true)]))
        .with(commitment(103, 1, 121, 134, true, &[]));
    assert_eq!(rotated.verdict(1), fail("type 103 base 121: null"));
    let stopped = stop(&rotated, &[reg(3)]).c(1);
    assert_eq!((stopped.verdict, stopped.n_checked), (Verdict::Pass, 2));
    assert_eq!(stopped.exemptions, ["type 103 base 121: null: chaos on reg3"]);

    for (size, min_size) in [(3, 0), (3, 4)] {
        let mut p = params(100, false, false);
        (p.size, p.min_size) = (size, min_size);
        let log = Log::run("mn,dkg", RunKind::StageA, &[p]);
        assert_eq!(log.verdict(1), incomplete("LlmqParams type 100 has no quorum size"));
    }
}
