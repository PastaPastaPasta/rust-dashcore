//! O12b-2's judge: the judged range, reorgs, provenance, chaos, C1 to C6 and both gates; and
//! O12b-2b's metrics and pages.

use std::collections::BTreeMap;
use std::ops::RangeInclusive;

use dash_testkit::mixnet::contract::{
    Binaries, Binary, Binding, Block, Caps, Chaos, CjSession, ClSeen, Clock, Commitment, Context,
    DigestCompare, DigestResult, DkgCase, DkgCaseKind, Envelope, Hex32, Host, Impl, IsReq, IsSeen,
    LlmqParams, Member, Miner, MnDiff, Preset, ProbeKind, ProbeRecovery, ProbeSkip, Proposal,
    Record, Reorg, RolePlan, Run, RunKind, SlotId, Superblock, Tip, TipStatus, V1, Watermark,
};
use dash_testkit::mixnet::criteria::{
    CjCount, Config, CriterionReport, Gate, Latency, Metrics, Owner, OwnerCount, OwnerRate, Page,
    PageKind, REHEARSAL_BANNER, Ratio, Report, Verdict, judge, metrics,
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
    let (size, min_size, threshold) = if rotation {
        (4, 4, 3)
    } else {
        (3, 2, 2)
    };
    LlmqParams {
        llmq_type,
        name: format!("llmq_{llmq_type}"),
        size,
        min_size,
        threshold,
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

    /// Inserts `record` just before the last watermark, where driver evidence counts.
    fn evidence(mut self, record: Record) -> Self {
        let last = self.records.iter().rposition(|r| matches!(r, Record::Watermark(_))).unwrap();
        self.records.insert(last, record);
        self
    }

    fn edit(mut self, f: impl Fn(&mut Record)) -> Self {
        self.records.iter_mut().for_each(f);
        self
    }

    /// Names `slot` as the miner of the block at `height`.
    fn mine(&mut self, height: u32, slot: SlotId) {
        let runs = self.plan.slot(slot).unwrap().runs;
        for r in &mut self.records {
            if let Record::Block(b) = r
                && b.height == height
            {
                b.miner = Some(Miner {
                    slot,
                    runs,
                });
            }
        }
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

fn none() -> Verdict {
    vacuous("no applicable instance")
}

/// A `mn,dkg` run: C1, C2 and C6 pass, C3 and C5 have no cap, and C4 has no case records.
fn clean_verdicts() -> [Verdict; 6] {
    let no_case = incomplete("contribution-omit: no rust target");
    [Verdict::Pass, Verdict::Pass, none(), no_case, none(), Verdict::Pass]
}

#[test]
fn a_clean_run_passes_c1_c2_and_c6_but_not_the_stage_a_gate() {
    let log = Log::clean();
    let report = log.judge();
    assert_eq!(verdicts(&report), clean_verdicts());
    let counts: Vec<_> = report.criteria.iter().map(|c| (c.n_applicable, c.n_checked)).collect();
    assert_eq!(counts, [(2, 2), (3, 3), (0, 0), (7, 0), (0, 0), (5, 5)]);
    assert!(report.criteria.iter().all(|c| c.exemptions.is_empty()));
    assert_eq!((report.gate, report.passed, report.banner), (Gate::StageA, false, None));
    let rehearsal = log.judge_with(Gate::Rehearsal, &Config::default());
    assert_eq!((rehearsal.passed, rehearsal.banner), (true, Some(REHEARSAL_BANNER)));
    assert_eq!(rehearsal.criteria, report.criteria);
    let v = serde_json::to_value(&report).unwrap();
    assert_eq!(v["gate"], "stage-a");
    assert_eq!(v["criteria"][0]["criterion"], "C1");
    assert_eq!(v["criteria"][0]["verdict"], json!({"status": "pass"}));
    assert_eq!(
        v["criteria"][2]["verdict"],
        json!({"status": "vacuous", "detail": "no applicable instance"})
    );
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
        [core_twin.clone(), core_twin.clone(), none(), core_twin, none(), Verdict::Pass]
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
        [
            rust_exe.clone(),
            rust_exe.clone(),
            rust_exe.clone(),
            rust_exe.clone(),
            rust_exe,
            Verdict::Pass
        ]
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

const ALL_CAPS: &str = "mn,dkg,sig,lock,gbt";

/// A probe of `kind` on the base-120 llmq_test quorum, isolated over `seqs`
/// (isolation start, request, result, isolation end).
fn probe(kind: ProbeKind, seqs: [u64; 4]) -> ProbeRecovery {
    ProbeRecovery {
        probe: kind,
        llmq_type: 100,
        quorum_hash: hash(0, 120),
        request_id: h(0x60 + kind as u8),
        msg_hash: h(0x68 + kind as u8),
        requested_seq: seqs[1],
        result_seq: seqs[2],
        isolation_start_seq: seqs[0],
        isolation_end_seq: seqs[3],
        stopped: vec![reg(1), reg(3)],
        verified: true,
        verified_quorum: Some(hash(0, 120)),
        elapsed_ms: 900,
        deadline_ms: 60_000,
    }
}

/// The base of case `i`'s quorum for a Rust (`half` 0) or dashd (`half` 1) target.
fn case_base(i: u32, half: u32) -> u32 {
    101 + 2 * i + half
}

fn proposal(height: u32, producer: SlotId, validator: SlotId) -> Proposal {
    Proposal {
        block: hash(0, height),
        height,
        producer,
        validator,
        result: None,
        tip: Some(TipStatus::Active),
    }
}

/// A Stage-A run with every cap and the evidence for all six criteria:
/// - C3: one probe of each kind, isolated across blocks 143-146;
/// - C4: each case on reg0 (Rust) and reg1 (dashd), each in its own type-104 quorum of all five
///   regular masternodes;
/// - C5: block 141 mined by reg0 and checked by node0, reg1 and reg3, and block 142 mined by
///   node0 and checked by reg0, reg2 and reg4.
fn full_with(types: &[LlmqParams]) -> Log {
    let mut log = Log::start(ALL_CAPS, RunKind::StageA, types);
    log.blocks(0, H0 + 1..=HN, 0);
    log.mine(141, reg(0));
    log.mine(142, SlotId::CONTROLLER);
    let seqs = [143, 144, 145, 146].map(|height| log.block_seq[&height]);
    for kind in ProbeKind::ALL {
        log.push(Record::ProbeRecovery(probe(kind, seqs)));
    }
    let everyone = [(0, true), (1, true), (2, true), (3, true), (4, true)];
    for (i, case) in (0..).zip(DkgCaseKind::ALL) {
        for (half, target) in [(0, reg(0)), (1, reg(1))] {
            let base = case_base(i, half);
            log.push(commitment(104, 0, base, base + 10, false, &everyone));
            log.push(Record::DkgCase(DkgCase {
                case,
                target,
                llmq_type: 104,
                quorum_hash: hash(0, base),
                complaints: 2,
                justifications: 1,
                pose_delta: 0,
            }));
        }
    }
    for validator in [SlotId::CONTROLLER, reg(1), reg(3)] {
        log.push(Record::Proposal(proposal(141, reg(0), validator)));
    }
    for validator in [reg(0), reg(2), reg(4)] {
        log.push(Record::Proposal(proposal(142, SlotId::CONTROLLER, validator)));
    }
    log.push(commitment(100, 0, 120, 131, false, &[(0, true), (1, true), (2, true)]));
    log.finish();
    log
}

fn full() -> Log {
    full_with(&[params(100, false, false)])
}

fn edit_probe(kind: ProbeKind, f: impl Fn(&mut ProbeRecovery)) -> Log {
    full().edit(|r| {
        if let Record::ProbeRecovery(p) = r
            && p.probe == kind
        {
            f(p);
        }
    })
}

fn edit_case(case: DkgCaseKind, target: SlotId, f: impl Fn(&mut DkgCase)) -> Log {
    full().edit(|r| {
        if let Record::DkgCase(d) = r
            && d.case == case
            && d.target == target
        {
            f(d);
        }
    })
}

/// `full()` with the commitment at `base` edited.
fn edit_quorum(base: u32, f: impl Fn(&mut Commitment)) -> Log {
    full().edit(|r| {
        if let Record::Commitment(c) = r
            && c.base_height == base
            && c.llmq_type == 104
        {
            f(c);
        }
    })
}

fn edit_proposal(height: u32, validator: SlotId, f: impl Fn(&mut Proposal)) -> Log {
    full().edit(|r| {
        if let Record::Proposal(p) = r
            && p.height == height
            && p.validator == validator
        {
            f(p);
        }
    })
}

#[test]
fn a_run_with_all_six_criteria_passes_the_stage_a_gate() {
    let report = full().judge();
    assert_eq!(verdicts(&report), vec![Verdict::Pass; 6]);
    assert!(report.passed);
    let counts: Vec<_> = report.criteria.iter().map(|c| (c.n_applicable, c.n_checked)).collect();
    assert_eq!(counts, [(2, 2), (3, 3), (3, 3), (7, 7), (6, 6), (5, 5)]);
    let mut rehearsal = full();
    if let Record::Run(run) = &mut rehearsal.records[0] {
        run.run_kind = RunKind::Rehearsal;
    }
    assert!(!rehearsal.judge().passed);
    assert!(rehearsal.rehearsal());
}

#[test]
fn c3_needs_a_fresh_isolated_verified_probe_of_each_enabled_kind() {
    let not_valid = |kind: &str, why: &str| incomplete(&format!("no valid {kind} probe ({why})"));
    let stale = edit_probe(ProbeKind::Recsig, |p| p.requested_seq = p.isolation_start_seq);
    assert_eq!(stale.verdict(3), not_valid("recsig", "stale or not isolated throughout"));
    let late = edit_probe(ProbeKind::Recsig, |p| p.isolation_end_seq = p.result_seq - 1);
    assert_eq!(late.verdict(3), not_valid("recsig", "stale or not isolated throughout"));
    let before = edit_probe(ProbeKind::Recsig, |p| p.result_seq = p.requested_seq - 1);
    assert_eq!(before.verdict(3), not_valid("recsig", "stale or not isolated throughout"));
    let running = edit_probe(ProbeKind::Chainlock, |p| p.stopped = vec![reg(1)]);
    assert_eq!(running.verdict(3), not_valid("chainlock", "reg3 not stopped"));
    let wrong = edit_probe(ProbeKind::Islock, |p| p.verified_quorum = Some(hash(1, 120)));
    assert_eq!(wrong.verdict(3), not_valid("islock", "wrong quorum"));
    let unnamed = edit_probe(ProbeKind::Islock, |p| p.verified_quorum = None);
    assert_eq!(unnamed.verdict(3), not_valid("islock", "wrong quorum"));
    let untyped = edit_probe(ProbeKind::Recsig, |p| p.llmq_type = 105);
    assert_eq!(untyped.verdict(3), not_valid("recsig", "no LlmqParams for type 105"));
    let elsewhere = edit_probe(ProbeKind::Recsig, |p| {
        p.quorum_hash = hash(0, 121);
        p.verified_quorum = Some(hash(0, 121));
    });
    assert_eq!(elsewhere.verdict(3), not_valid("recsig", "no final commitment"));
    let mut strict = params(100, false, false);
    strict.threshold = 3;
    assert_eq!(
        full_with(&[strict]).verdict(3),
        not_valid("recsig", "2 Rust valid members, threshold 3")
    );

    let id = |kind| h(0x60 + kind as u8);
    let unverified = edit_probe(ProbeKind::Chainlock, |p| p.verified = false);
    assert_eq!(
        unverified.verdict(3),
        fail(&format!("chainlock {}: not verified", id(ProbeKind::Chainlock)))
    );
    let slow = edit_probe(ProbeKind::Islock, |p| p.elapsed_ms = p.deadline_ms + 1);
    assert_eq!(
        slow.verdict(3),
        fail(&format!("islock {}: deadline missed", id(ProbeKind::Islock)))
    );
    let on_time = edit_probe(ProbeKind::Islock, |p| p.elapsed_ms = p.deadline_ms);
    assert_eq!(on_time.verdict(3), Verdict::Pass);

    let is_islock =
        |r: &Record| matches!(r, Record::ProbeRecovery(p) if p.probe == ProbeKind::Islock);
    let missing = full().without(is_islock);
    assert_eq!(missing.verdict(3), incomplete("no valid islock probe"));
    let skipped = missing.clone().evidence(Record::ProbeSkip(ProbeSkip {
        probe: ProbeKind::Islock,
        reason: "infeasible".into(),
    }));
    assert_eq!(skipped.verdict(3), Verdict::NotRun("islock: infeasible".into()));
    let seqs = [143, 144, 145, 146].map(|height| full().block_seq[&height]);
    let after_watermark = missing.with(Record::ProbeRecovery(probe(ProbeKind::Islock, seqs)));
    assert_eq!(after_watermark.verdict(3), incomplete("no valid islock probe"));
    let stale_second = edit_probe(ProbeKind::Islock, |p| {
        p.probe = ProbeKind::Recsig;
        p.requested_seq = p.isolation_start_seq;
    });
    assert_eq!(stale_second.verdict(3), incomplete("no valid islock probe"), "recsig still counts");
    let ignored_skip = full().without(is_islock).with(Record::ProbeSkip(ProbeSkip {
        probe: ProbeKind::Islock,
        reason: "infeasible".into(),
    }));
    assert_eq!(ignored_skip.verdict(3), incomplete("no valid islock probe"), "after the watermark");
    let counted_wins = full().evidence(Record::ProbeSkip(ProbeSkip {
        probe: ProbeKind::Islock,
        reason: "infeasible".into(),
    }));
    assert_eq!(counted_wins.verdict(3), Verdict::Pass);
    let seqs = [143, 144, 145, 146].map(|height| full().block_seq[&height]);
    let second = full().evidence(Record::ProbeRecovery(ProbeRecovery {
        verified: false,
        ..probe(ProbeKind::Islock, seqs)
    }));
    assert_eq!(second.verdict(3), fail(&format!("islock {}: not verified", id(ProbeKind::Islock))));
    let edges = edit_probe(ProbeKind::Recsig, |p| {
        p.result_seq = p.requested_seq;
        p.isolation_end_seq = p.result_seq;
    });
    assert_eq!(edges.verdict(3), Verdict::Pass);
    let c3_quorum = |f: fn(&mut Commitment)| {
        full().edit(move |r| {
            if let Record::Commitment(c) = r
                && c.llmq_type == 100
            {
                f(c);
            }
        })
    };
    let invalid_rust = c3_quorum(|c| c.members[0].valid = false);
    assert_eq!(invalid_rust.verdict(3), not_valid("recsig", "1 Rust valid members, threshold 2"));
    let null_quorum = c3_quorum(|c| c.null = true);
    assert_eq!(null_quorum.verdict(3), not_valid("recsig", "no final commitment"));

    let mut sig_only = Log::start("mn,dkg,sig", RunKind::StageA, &[params(100, false, false)]);
    sig_only.blocks(0, H0 + 1..=HN, 0);
    let seqs = [143, 144, 145, 146].map(|height| sig_only.block_seq[&height]);
    sig_only.push(Record::ProbeRecovery(probe(ProbeKind::Recsig, seqs)));
    sig_only.push(commitment(100, 0, 120, 131, false, &[(0, true), (1, true), (2, true)]));
    sig_only.finish();
    assert_eq!((sig_only.verdict(3), sig_only.c(3).n_applicable), (Verdict::Pass, 1));
}

#[test]
fn c4_compares_the_rust_and_dashd_halves_of_every_case() {
    use DkgCaseKind::*;
    let gone = full()
        .without(|r| matches!(r, Record::DkgCase(d) if d.case == JustifyLie && d.target == reg(0)));
    assert_eq!(gone.verdict(4), incomplete("justify-lie: no rust target"));
    let twice = full().evidence(Record::DkgCase(DkgCase {
        case: ContributionLie,
        target: reg(3),
        llmq_type: 104,
        quorum_hash: hash(0, case_base(1, 1)),
        complaints: 2,
        justifications: 1,
        pose_delta: 0,
    }));
    assert_eq!(twice.verdict(4), incomplete("contribution-lie: 2 dashd targets"));
    let late_twice = full().with(Record::DkgCase(DkgCase {
        case: ContributionLie,
        target: reg(3),
        llmq_type: 104,
        quorum_hash: hash(0, case_base(1, 1)),
        complaints: 2,
        justifications: 1,
        pose_delta: 0,
    }));
    assert_eq!(late_twice.verdict(4), Verdict::Pass, "after the watermark");
    let node = edit_case(CommitLie, reg(1), |d| d.target = SlotId::CONTROLLER);
    assert_eq!(node.verdict(4), incomplete("commit-lie: no dashd target"));

    let complaints = edit_case(CommitLie, reg(1), |d| d.complaints = 3);
    assert_eq!(complaints.verdict(4), fail("commit-lie: complaints rust=2 dashd=3"));
    let justified = edit_case(ComplainLie, reg(0), |d| d.justifications = 2);
    assert_eq!(justified.verdict(4), fail("complain-lie: justifications rust=2 dashd=1"));
    let punished = edit_case(JustifyOmit, reg(0), |d| d.pose_delta = 66);
    assert_eq!(punished.verdict(4), fail("justify-omit: pose_delta rust=66 dashd=0"));
    let both = punished.edit(|r| {
        if let Record::DkgCase(d) = r
            && d.case == ContributionOmit
            && d.target == reg(1)
        {
            d.complaints = 9;
        }
    });
    assert_eq!(both.verdict(4), fail("contribution-omit: complaints rust=2 dashd=9"));

    let invalid_target = edit_quorum(case_base(0, 0), |c| c.members[0].valid = false);
    assert_eq!(invalid_target.verdict(4), fail("contribution-omit: target_valid rust=0 dashd=1"));
    let invalid_other = edit_quorum(case_base(0, 1), |c| c.members[3].valid = false);
    assert_eq!(invalid_other.verdict(4), fail("contribution-omit: others_valid rust=4 dashd=3"));
    let smaller = edit_quorum(case_base(0, 1), |c| drop(c.members.pop()));
    assert_eq!(smaller.verdict(4), fail("contribution-omit: others_valid rust=4 dashd=3"));
    let larger = edit_quorum(case_base(0, 1), |c| {
        c.members.push(Member {
            pro_tx: h(0x7e),
            valid: false,
        })
    });
    assert_eq!(larger.verdict(4), fail("contribution-omit: members rust=5 dashd=6"));
    let null = edit_quorum(case_base(0, 1), |c| {
        c.null = true;
        c.members.clear();
    });
    assert_eq!(null.verdict(4), fail("contribution-omit: null rust=0 dashd=1"));
    let both_null = null.edit(|r| {
        if let Record::Commitment(c) = r
            && c.llmq_type == 104
            && c.base_height == case_base(0, 0)
        {
            c.null = true;
            c.members.clear();
        }
    });
    assert_eq!(both_null.verdict(4), Verdict::Pass);
    let reordered = edit_quorum(case_base(2, 1), |c| c.members.rotate_left(2));
    assert_eq!(reordered.verdict(4), Verdict::Pass);

    let outside = edit_quorum(case_base(0, 0), |c| drop(c.members.remove(0)));
    assert_eq!(
        outside.verdict(4),
        incomplete("contribution-omit: the rust target is not a member")
    );
    let unknown = edit_case(ContributionOmit, reg(0), |d| d.quorum_hash = hash(0, 99));
    assert_eq!(
        unknown.verdict(4),
        incomplete("contribution-omit: no commitment for the rust target's quorum")
    );
    let off_chain = edit_quorum(case_base(0, 0), |c| c.quorum_hash = hash(1, case_base(0, 0)))
        .edit(|r| {
            if let Record::DkgCase(d) = r
                && d.quorum_hash == hash(0, case_base(0, 0))
            {
                d.quorum_hash = hash(1, case_base(0, 0));
            }
        });
    assert_eq!(
        off_chain.verdict(4),
        incomplete("contribution-omit: no commitment for the rust target's quorum")
    );
}

#[test]
fn c5_needs_both_producer_directions_accepted_by_every_opposite_validator() {
    let at = |height, validator: &str| {
        let dir = if height == 141 {
            "rust -> dashd"
        } else {
            "dashd -> rust"
        };
        format!("{dir}: block {} at {height} on {validator}", hash(0, height))
    };
    let set = |result: Option<&str>, tip| {
        edit_proposal(141, reg(3), |p| {
            p.result = result.map(String::from);
            p.tip = tip;
        })
        .verdict(5)
    };
    assert_eq!(set(Some("duplicate"), Some(TipStatus::Active)), Verdict::Pass);
    assert_eq!(set(None, Some(TipStatus::ValidFork)), Verdict::Pass);
    assert_eq!(
        set(Some("duplicate-invalid"), Some(TipStatus::Active)),
        fail(&format!("{}: rejected: duplicate-invalid", at(141, "reg3")))
    );
    assert_eq!(
        set(Some("bad-cb-amount"), None),
        fail(&format!("{}: rejected: bad-cb-amount", at(141, "reg3")))
    );
    assert_eq!(
        set(Some("duplicate"), Some(TipStatus::Invalid)),
        fail(&format!("{}: invalid tip", at(141, "reg3")))
    );
    assert_eq!(
        set(Some("duplicate-inconclusive"), Some(TipStatus::Active)),
        incomplete(&format!("{}: duplicate-inconclusive", at(141, "reg3")))
    );
    assert_eq!(
        set(Some("inconclusive-not-best-prevblk"), Some(TipStatus::Active)),
        incomplete(&format!("{}: inconclusive-not-best-prevblk", at(141, "reg3")))
    );
    assert_eq!(set(None, None), incomplete(&format!("{}: tip None", at(141, "reg3"))));
    assert_eq!(
        set(None, Some(TipStatus::HeadersOnly)),
        incomplete(&format!("{}: tip Some(HeadersOnly)", at(141, "reg3")))
    );

    let produced_by =
        |height: u32| move |r: &Record| matches!(r, Record::Proposal(p) if p.height == height);
    assert_eq!(full().without(produced_by(142)).verdict(5), incomplete("dashd -> rust: no block"));
    let unchecked = full().without(|r| matches!(r, Record::Proposal(p) if p.validator == reg(3)));
    assert_eq!(unchecked.verdict(5), incomplete("rust -> dashd: no proposal to reg3"));
    let wrong_miner = full().edit(|r| {
        if let Record::Proposal(p) = r
            && p.height == 141
        {
            p.producer = reg(2);
        }
    });
    assert_eq!(
        wrong_miner.verdict(5),
        incomplete(&format!("{}: its block record names another miner", at(141, "node0")))
    );
    let unrecorded = edit_proposal(141, reg(3), |p| p.block = hash(1, 141));
    let at_unrecorded = format!("rust -> dashd: block {} at 141 on reg3", hash(1, 141));
    assert_eq!(unrecorded.verdict(5), incomplete(&format!("{at_unrecorded}: no block record")));
    let misplaced = edit_proposal(141, reg(3), |p| p.height = 140);
    let at_misplaced = format!("rust -> dashd: block {} at 140 on reg3", hash(0, 141));
    assert_eq!(
        misplaced.verdict(5),
        incomplete(&format!("{at_misplaced}: its block record is at 141"))
    );
    let unchecked_back =
        full().without(|r| matches!(r, Record::Proposal(p) if p.validator == reg(4)));
    assert_eq!(unchecked_back.verdict(5), incomplete("dashd -> rust: no proposal to reg4"));
    let late_reject = full().with(Record::Proposal(Proposal {
        result: Some("bad-cb".into()),
        ..proposal(141, reg(0), reg(3))
    }));
    assert_eq!(late_reject.verdict(5), Verdict::Pass, "after the watermark");
    let same_impl = full().evidence(Record::Proposal(Proposal {
        result: Some("bad-txns".into()),
        ..proposal(141, reg(0), reg(2))
    }));
    assert_eq!(same_impl.verdict(5), Verdict::Pass);

    let rejected = edit_proposal(141, SlotId::CONTROLLER, |p| p.result = Some("bad-cb".into()));
    let around = |log: &Log, slot| {
        let seq = |height| log.block_seq[&height];
        log.clone().with(chaos("c5", slot, seq(140), seq(142)))
    };
    let exempt = around(&rejected, SlotId::CONTROLLER).c(5);
    assert_eq!((exempt.verdict, exempt.n_applicable, exempt.n_checked), (Verdict::Pass, 6, 2));
    assert_eq!(exempt.exemptions.len(), 4);
    let other = around(&rejected, reg(4)).c(5);
    assert!(matches!(other.verdict, Verdict::Fail(_)));
    assert_eq!(other.exemptions.len(), 1);
    let strict = Config {
        chaos_allow: Some(0),
        ..Config::default()
    };
    let seq = |height| rejected.block_seq[&height];
    let earlier = rejected.clone().with(chaos("c5", SlotId::CONTROLLER, seq(110), seq(112)));
    assert!(matches!(
        earlier.judge_with(Gate::StageA, &strict).criteria[4].verdict,
        Verdict::Fail(_)
    ));
    let moved = edit_proposal(141, SlotId::CONTROLLER, |p| {
        p.result = Some("bad-cb".into());
        p.height = 120;
    });
    let seq = |height| moved.block_seq[&height];
    let covered = moved.clone().with(chaos("c5", SlotId::CONTROLLER, seq(119), seq(121)));
    assert_eq!(
        covered.judge_with(Gate::StageA, &strict).criteria[4].verdict,
        incomplete(&format!(
            "rust -> dashd: block {} at 120 on node0: its block record is at 141",
            hash(0, 141)
        )),
        "the record is checked before any exemption"
    );

    let mut no_gbt = Log::start("mn,dkg,sig,lock", RunKind::StageA, &[params(100, false, false)]);
    no_gbt.blocks(0, H0 + 1..=HN, 0);
    no_gbt.finish();
    assert_eq!(no_gbt.verdict(5), none());
}

const SECOND: u64 = 1_000_000_000;

/// O12b-2b's metrics of `log`, with record `i` written at `i + 1` seconds.
fn measure(log: &Log) -> Metrics {
    let mut lines = log.envelopes();
    for e in &mut lines {
        e.mono_ns = e.seq * SECOND;
    }
    metrics(&lines, &Config::default()).unwrap()
}

fn ratio(num: u64, den: u64) -> Ratio {
    Ratio {
        num,
        den,
    }
}

fn rate(owner: Owner, num: u64, den: u64) -> OwnerRate {
    OwnerRate {
        owner,
        rate: ratio(num, den),
    }
}

fn cl(node: SlotId, height: u32, tag: u8) -> Record {
    Record::ClSeen(ClSeen {
        node,
        height,
        hash: hash(tag, height),
    })
}

fn is_seen(node: SlotId, txid: Hex32) -> Record {
    Record::IsSeen(IsSeen {
        node,
        txid,
    })
}

fn superblock(height: u32, creator: SlotId, paid: bool) -> Record {
    Record::Superblock(Superblock {
        height,
        trigger_hash: hash(0x5b, height),
        creator_pro_tx: pro(creator),
        paid,
    })
}

fn tip(node: SlotId, height: u32, tag: u8) -> Record {
    Record::Tip(Tip {
        node,
        height,
        hash: hash(tag, height),
    })
}

fn pages(m: &Metrics) -> Vec<(PageKind, u32)> {
    m.pages.iter().map(|p| (p.kind, p.height)).collect()
}

#[test]
fn metrics_rate_dkgs_per_type_and_impl() {
    let clean = measure(&Log::clean());
    assert_eq!(clean.range, (H0, HN));
    assert_eq!(clean.dkg.len(), 1);
    let d = &clean.dkg[0];
    assert_eq!((d.llmq_type, d.final_), (100, ratio(1, 1)));
    assert_eq!(d.by_impl, [rate(Owner::Rust, 1, 1), rate(Owner::Dashd, 1, 1)]);
    assert_eq!(clean.rust_share, ratio(2, 3));
    let json = serde_json::to_value(&clean).unwrap();
    assert_eq!(json["dkg"][0]["final"], json!({"num": 1, "den": 1}));

    let null = measure(&Log::no_commitment().with(commitment(100, 0, 120, 131, true, &[])));
    assert_eq!(null.dkg[0].final_, ratio(0, 1));
    assert_eq!(null.dkg[0].by_impl, [rate(Owner::Rust, 0, 1), rate(Owner::Dashd, 0, 1)]);
    assert_eq!(null.rust_share, ratio(0, 0));
    let missing = measure(&Log::no_commitment());
    assert_eq!(missing.dkg[0].final_, ratio(0, 1));

    let dashd_only = measure(&Log::no_commitment().with(commitment(
        100,
        0,
        120,
        131,
        false,
        &[(0, false), (1, true), (2, false)],
    )));
    assert_eq!(dashd_only.dkg[0].by_impl, [rate(Owner::Rust, 0, 1), rate(Owner::Dashd, 1, 1)]);
    assert_eq!(dashd_only.rust_share, ratio(0, 1));

    let evo = measure(&Log::run(
        "mn,dkg",
        RunKind::StageA,
        &[params(100, false, false), params(102, false, true), params(103, true, false)],
    ));
    assert_eq!(evo.dkg[1].llmq_type, 102);
    assert_eq!((evo.dkg[1].final_, evo.dkg[1].by_impl.len()), (ratio(0, 1), 0));
    assert_eq!((evo.dkg[2].llmq_type, evo.dkg[2].final_), (103, ratio(0, 2)));

    let mut no_params = Log::clean();
    no_params.records.retain(|r| !matches!(r, Record::LlmqParams(_)));
    let mut lines = no_params.envelopes();
    lines.iter_mut().for_each(|e| e.mono_ns = e.seq);
    assert_eq!(metrics(&lines, &Config::default()), Err("no LlmqParams record".into()));
}

#[test]
fn metrics_time_node0_chainlocks_from_their_blocks() {
    let mut log = Log::start("mn,dkg,lock", RunKind::StageA, &[params(100, false, false)]);
    log.push(cl(SlotId::CONTROLLER, 101, 0));
    for height in H0 + 1..=HN {
        log.blocks(0, height..=height, 0);
        if height % 2 == 0 {
            log.push(cl(SlotId::CONTROLLER, height, 0));
        }
        if height % 10 == 0 {
            log.push(cl(reg(1), height + 1, 0));
            log.push(cl(SlotId::CONTROLLER, height + 1, 1));
        }
    }
    log.finish();
    let m = measure(&log);
    assert_eq!(m.chainlocks.coverage, ratio(26, 50));
    assert_eq!(m.chainlocks.unordered, 1);
    let second = Latency {
        n: 25,
        p50_ms: 1000,
        p95_ms: 1000,
        max_ms: 1000,
    };
    assert_eq!(m.chainlocks.latency, second);
    assert_eq!(pages(&m), [111, 121, 131, 141, 151].map(|h| (PageKind::ClConflict, h)));
    assert_eq!(m.pages[0].detail, format!("{}, {}", hash(0, 111), hash(1, 111)));
}

#[test]
fn metrics_count_instantsend_timeouts_and_pending_requests() {
    let mut log = Log::clean();
    let req = |n: u8| {
        Record::IsReq(IsReq {
            txid: h(0xa0 + n),
        })
    };
    let pad = |log: &mut Log, n: usize| {
        for _ in 0..n {
            log.push(is_seen(reg(1), h(0xee)));
        }
    };
    log.push(is_seen(SlotId::CONTROLLER, h(0xa4)));
    log.push(req(4));
    log.push(req(0));
    log.push(is_seen(reg(2), h(0xa0)));
    log.push(is_seen(SlotId::CONTROLLER, h(0xa0)));
    log.push(req(0));
    log.push(req(1));
    log.push(req(2));
    pad(&mut log, 60);
    log.push(is_seen(SlotId::CONTROLLER, h(0xa1)));
    log.push(req(3));
    pad(&mut log, 5);
    let m = measure(&log).instantsend;
    assert_eq!((m.success, m.pending), (ratio(2, 4), 1));
    let latency = Latency {
        n: 3,
        p50_ms: 2000,
        p95_ms: 62_000,
        max_ms: 62_000,
    };
    assert_eq!(m.latency, latency);

    let mut edges = Log::clean();
    edges.push(req(5));
    pad(&mut edges, 59);
    edges.push(is_seen(SlotId::CONTROLLER, h(0xa5)));
    edges.push(req(6));
    pad(&mut edges, 60);
    let m = measure(&edges).instantsend;
    assert_eq!((m.success, m.pending, m.latency.max_ms), (ratio(1, 2), 0, 60_000));
}

#[test]
fn metrics_count_pose_coinjoin_and_superblocks_by_impl() {
    let log = Log::clean()
        .with(row(reg(0), 130, Some(66), None))
        .with(row(reg(0), 131, Some(65), Some(131)))
        .with(row(reg(0), 135, None, Some(131)))
        .with(row(reg(0), 140, Some(100), None))
        .with(row(reg(4), 130, Some(66), None))
        .with(row(reg(4), 133, Some(66), None))
        .with(row(reg(1), 135, Some(66), None))
        .with(row(reg(4), 90, Some(66), None))
        .with(row(reg(2), 141, Some(66), None));
    let seq = |height| log.block_seq[&height];
    let log = log.clone().with(chaos("stop-2", reg(2), seq(140), seq(142)));
    let m = measure(&log);
    let count = |owner, n| OwnerCount {
        owner,
        n,
    };
    assert_eq!(m.pose, [count(Owner::Rust, 4), count(Owner::Dashd, 1)]);
    assert_eq!(pages(&m), [(PageKind::Pose, 130)]);
    assert_eq!(
        m.pages[0].detail,
        format!(
            "{} (slot reg0) at 130: penalty rose to 66; {} (slot reg4) at 130: penalty rose to 66",
            pro(reg(0)),
            pro(reg(4))
        )
    );
    assert_eq!(
        m.exempt,
        [format!("{} (slot reg2) at 141: penalty rose to 66: chaos stop-2", pro(reg(2)))]
    );

    let unusable = log.clone().with(chaos("stop-3", reg(2), seq(140), 10_000));
    let m = measure(&unusable);
    assert_eq!(pages(&m), [(PageKind::Pose, 130), (PageKind::Pose, 141)]);
    assert!(m.exempt.is_empty());

    let session = |coordinator, outcome: &str| {
        Record::CjSession(CjSession {
            wallet: "w".into(),
            coordinator_pro_tx: coordinator,
            outcome: outcome.into(),
        })
    };
    let m = measure(
        &Log::clean()
            .with(session(pro(reg(0)), "success"))
            .with(session(pro(reg(1)), "timeout"))
            .with(session(pro(reg(0)), "success"))
            .with(session(h(0x99), "success"))
            .with(superblock(125, reg(2), true))
            .with(superblock(149, reg(3), false))
            .with(superblock(90, reg(1), true)),
    );
    let cj = |owner, outcome: &str, n| CjCount {
        owner,
        outcome: outcome.into(),
        n,
    };
    assert_eq!(
        m.coinjoin,
        [
            cj(Owner::Rust, "success", 2),
            cj(Owner::Dashd, "timeout", 1),
            cj(Owner::Unbound, "success", 1)
        ]
    );
    assert_eq!(m.superblocks, [rate(Owner::Rust, 1, 1), rate(Owner::Dashd, 0, 1)]);

    let mut moved = Log::clean().with(session(h(0x3f), "success"));
    for (slot, exe) in [(reg(1), 1), (reg(2), 2)] {
        let from_seq = moved.records.len() as u64 + 1;
        let role = moved.plan.slot(slot).unwrap();
        let (label, runs) = (role.label, role.runs);
        moved.push(Record::Binding(Binding {
            slot,
            pro_tx: h(0x3f),
            label,
            runs,
            exe_sha256: h(exe),
            from_height: 140,
            from_seq,
        }));
    }
    let m = measure(&moved.with(session(h(0x3f), "success")));
    assert_eq!(m.coinjoin, [cj(Owner::Rust, "success", 1), cj(Owner::Unbound, "success", 1)]);
}

#[test]
fn metrics_page_digest_and_best_hash_divergence_once() {
    let mismatch = |slot| {
        digest(
            slot,
            H0 + 1,
            HN - 6,
            DigestResult::Mismatch {
                height: 130,
            },
        )
    };
    let log = Log::clean()
        .without(digests_of(reg(3)))
        .without(digests_of(reg(1)))
        .with(mismatch(reg(3)))
        .with(mismatch(reg(1)))
        .with(tip(reg(1), 120, 1))
        .with(tip(reg(1), 120, 1))
        .with(tip(reg(3), 140, 0))
        .with(tip(reg(3), 146, 1))
        .with(tip(reg(3), 144, 1))
        .with(tip(reg(3), 145, 1));
    let m = measure(&log);
    assert_eq!(
        pages(&m),
        [(PageKind::BestHash, 120), (PageKind::DigestMismatch, 130), (PageKind::BestHash, 144)]
    );
    assert_eq!(m.pages[1].detail, "reg3; reg1");
    assert_eq!(
        m.pages[0].detail,
        format!("reg1 at 120: {}, reference {}", hash(1, 120), hash(0, 120))
    );

    let mut flapping = Log::start("mn,dkg", RunKind::StageA, &[params(100, false, false)]);
    for height in H0 + 1..=HN {
        flapping.blocks(0, height..=height, 0);
        flapping.push(tip(
            reg(1),
            height,
            if height % 2 == 0 {
                1
            } else {
                0
            },
        ));
        flapping.push(tip(reg(1), height, 0));
    }
    flapping.finish();
    assert_eq!(measure(&flapping).pages, Vec::<Page>::new());
    assert_eq!(measure(&Log::clean()).pages, Vec::<Page>::new());
}

#[test]
fn metrics_drop_what_a_reorg_retracted() {
    let (log, _) = reorged(superblock(132, reg(0), true), superblock(140, reg(1), true));
    assert_eq!(measure(&log).superblocks, [rate(Owner::Dashd, 1, 1)]);
    let (log, _) = reorged(cl(SlotId::CONTROLLER, 132, 0), cl(SlotId::CONTROLLER, 132, 1));
    let m = measure(&log);
    assert_eq!(m.chainlocks.coverage, ratio(1, 50));
    assert_eq!(pages(&m), [(PageKind::ClConflict, 132)]);
}
