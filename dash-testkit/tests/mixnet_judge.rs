//! O12b-3a's `mixnet-judge` CLI: exit codes, JSON output, plan matching and argument checks.

use std::collections::BTreeMap;
use std::fs;
use std::ops::RangeInclusive;
use std::path::Path;
use std::process::{Command, Output};

use dash_testkit::mixnet::contract::{
    Binaries, Binary, Binding, Block, Caps, Clock, Commitment, Context, DigestCompare,
    DigestResult, DkgCase, DkgCaseKind, Envelope, Hex32, Host, Impl, LlmqParams, Member, Miner,
    MnDiff, Preset, ProbeKind, ProbeRecovery, Proposal, Record, RolePlan, Run, RunKind, SlotId,
    TipStatus, V1, Watermark,
};
use dash_testkit::mixnet::criteria::REHEARSAL_BANNER;
use serde_json::{Value, json};
use tempfile::TempDir;

const H0: u32 = 100;
const HN: u32 = 150;
const ALL_CAPS: &str = "mn,dkg,sig,lock,gbt";
const USAGE: &str = "usage: mixnet-judge <obs.jsonl> --plan <plan.json> --gate <stage-a|rehearsal>";

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

fn params(llmq_type: u8) -> LlmqParams {
    LlmqParams {
        llmq_type,
        name: format!("llmq_{llmq_type}"),
        size: 3,
        min_size: 2,
        threshold: 2,
        dkg_interval: 24,
        mining_window_start: 10,
        mining_window_end: 18,
        rotation: false,
        signing_active_quorum_count: 2,
        evo_only: false,
    }
}

fn pose(pro_tx: Hex32, height: u32, penalty: u32, ban: i32) -> Record {
    Record::MnDiff(MnDiff {
        height,
        pro_tx,
        pose_penalty: Some(penalty),
        pose_ban_height: Some(ban),
        pose_revived_height: Some(-1),
    })
}

fn commitment(llmq_type: u8, index: u16, base: u32, mined: u32, members: &[(u16, bool)]) -> Record {
    Record::Commitment(Commitment {
        llmq_type,
        quorum_hash: hash(0, base),
        index,
        base_height: base,
        mined_height: mined,
        null: false,
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

#[derive(Clone)]
struct Log {
    records: Vec<Record>,
    plan: RolePlan,
    block_seq: BTreeMap<u32, u64>,
}

impl Log {
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
            log.push(pose(pro(s.slot), H0, 0, -1));
        }
        log
    }

    fn full() -> Self {
        let mut log = Log::start(ALL_CAPS, RunKind::StageA, &[params(100)]);
        log.blocks(H0 + 1..=HN);
        log.mine(141, reg(0));
        log.mine(142, SlotId::CONTROLLER);
        let seqs = [143, 144, 145, 146].map(|height| log.block_seq[&height]);
        for kind in ProbeKind::ALL {
            log.push(Record::ProbeRecovery(ProbeRecovery {
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
            }));
        }
        let everyone = [(0, true), (1, true), (2, true), (3, true), (4, true)];
        for (i, case) in (0u32..).zip(DkgCaseKind::ALL) {
            for (half, target) in [(0u32, reg(0)), (1, reg(1))] {
                let base = 101 + 2 * i + half;
                log.push(commitment(104, 0, base, base + 10, &everyone));
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
            log.push(Record::Proposal(Proposal {
                block: hash(0, 141),
                height: 141,
                producer: reg(0),
                validator,
                result: None,
                tip: Some(TipStatus::Active),
            }));
        }
        for validator in [reg(0), reg(2), reg(4)] {
            log.push(Record::Proposal(Proposal {
                block: hash(0, 142),
                height: 142,
                producer: SlotId::CONTROLLER,
                validator,
                result: None,
                tip: Some(TipStatus::Active),
            }));
        }
        log.push(commitment(100, 0, 120, 131, &[(0, true), (1, true), (2, true)]));
        log.finish();
        log
    }

    fn push(&mut self, record: Record) {
        if let Record::Block(b) = &record {
            self.block_seq.insert(b.height, self.records.len() as u64 + 1);
        }
        self.records.push(record);
    }

    fn blocks(&mut self, heights: RangeInclusive<u32>) {
        for height in heights {
            self.push(Record::Block(Block {
                height,
                hash: hash(0, height),
                prev: hash(0, height - 1),
                miner: None,
            }));
        }
    }

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

    fn finish(&mut self) {
        self.push(Record::Watermark(Watermark {
            height: HN,
        }));
        let nodes: Vec<SlotId> =
            self.plan.slots.iter().map(|s| s.slot).filter(|&s| s != SlotId::CONTROLLER).collect();
        for slot in nodes {
            self.push(Record::DigestCompare(DigestCompare {
                node: slot,
                from_height: H0 + 1,
                to_height: HN - 6,
                result: DigestResult::Equal,
            }));
        }
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

    fn jsonl(&self) -> String {
        self.envelopes()
            .iter()
            .map(|e| serde_json::to_string(e).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    }
}

fn write_plan(dir: &TempDir, name: &str, plan: &RolePlan) -> std::path::PathBuf {
    let path = dir.path().join(name);
    fs::write(&path, serde_json::to_string_pretty(plan).unwrap()).unwrap();
    path
}

fn write_log(dir: &TempDir, name: &str, text: &str) -> std::path::PathBuf {
    let path = dir.path().join(name);
    fs::write(&path, text).unwrap();
    path
}

fn run_bin(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mixnet-judge")).args(args).output().unwrap()
}

fn run_judge(obs: &Path, plan: &Path, gate: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mixnet-judge"))
        .arg(obs)
        .arg("--plan")
        .arg(plan)
        .arg("--gate")
        .arg(gate)
        .output()
        .unwrap()
}

fn is_null_field(v: &Value, key: &str) -> bool {
    v.get(key).is_some_and(Value::is_null)
}

fn all_incomplete(v: &Value) -> bool {
    let criteria = v["report"]["criteria"].as_array().unwrap();
    criteria.len() == 6 && criteria.iter().all(|c| c["verdict"]["status"] == "incomplete")
}

fn parse_stdout(out: &Output) -> Value {
    let text = String::from_utf8(out.stdout.clone()).unwrap();
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("invalid stdout JSON: {e}\n{text}"))
}

#[test]
fn passing_stage_a_log_exits_0_with_report_and_metrics() {
    let dir = TempDir::new().unwrap();
    let log = Log::full();
    let plan = write_plan(&dir, "plan.json", &log.plan);
    let obs = write_log(&dir, "obs.jsonl", &format!("\n\u{a0}\n\u{b}\n{}\n   \n", log.jsonl()));

    let out = run_judge(&obs, &plan, "stage-a");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(out.stderr.is_empty());

    let v = parse_stdout(&out);
    assert_eq!(v["report"]["gate"], "stage-a");
    assert_eq!(v["report"]["passed"], true);
    assert!(is_null_field(&v["report"], "banner"));
    assert_eq!(v["metrics"]["range"], json!([H0, HN]));
    assert!(is_null_field(&v, "metrics_error"));
}

#[test]
fn fail_verdict_exits_1() {
    let dir = TempDir::new().unwrap();
    let mut log = Log::full();
    log.push(Record::MnDiff(MnDiff {
        height: 135,
        pro_tx: pro(reg(2)),
        pose_penalty: Some(66),
        pose_ban_height: None,
        pose_revived_height: None,
    }));
    let plan = write_plan(&dir, "plan.json", &log.plan);
    let obs = write_log(&dir, "obs.jsonl", &log.jsonl());

    let out = run_judge(&obs, &plan, "stage-a");
    assert_eq!(out.status.code(), Some(1), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let v = parse_stdout(&out);
    assert_eq!(v["report"]["passed"], false);
    assert_eq!(v["report"]["criteria"][1]["verdict"]["status"], "fail");
    assert_eq!(v["metrics"]["range"], json!([H0, HN]));
    assert!(is_null_field(&v, "metrics_error"));
}

#[test]
fn incomplete_only_logs_exit_2() {
    let dir = TempDir::new().unwrap();
    let full = Log::full();
    let plan = write_plan(&dir, "plan.json", &full.plan);

    // Empty log (blank lines only).
    let empty = write_log(&dir, "empty.jsonl", "\n   \n");
    let out = run_judge(&empty, &plan, "stage-a");
    assert_eq!(out.status.code(), Some(2));
    let v = parse_stdout(&out);
    assert_eq!(v["report"]["passed"], false);
    assert!(all_incomplete(&v));
    assert!(is_null_field(&v, "metrics"));
    assert!(v["metrics_error"].as_str().is_some_and(|s| !s.is_empty()));

    // First record is not Run: skips plan comparison and reports Incomplete everywhere.
    let no_run = Envelope {
        v: V1,
        run: "r".into(),
        seq: 1,
        mono_ns: 0,
        src: "watch".into(),
        record: Record::Watermark(Watermark {
            height: H0,
        }),
    };
    let no_run_path = write_log(&dir, "no_run.jsonl", &serde_json::to_string(&no_run).unwrap());
    let out = run_judge(&no_run_path, &plan, "stage-a");
    assert_eq!(out.status.code(), Some(2));
    let v = parse_stdout(&out);
    assert_eq!(v["report"]["passed"], false);
    assert!(all_incomplete(&v));
    assert!(is_null_field(&v, "metrics"));
    assert!(v["metrics_error"].as_str().is_some_and(|s| s.contains("not a Run")));

    // Run whose preset/context fails RolePlan::new: skips plan comparison and reports Incomplete.
    let mut bad_run = full.envelopes()[0].clone();
    if let Record::Run(run) = &mut bad_run.record {
        run.context = Context::Devnet;
    }
    let bad_run_path = write_log(&dir, "bad_run.jsonl", &serde_json::to_string(&bad_run).unwrap());
    let out = run_judge(&bad_run_path, &plan, "stage-a");
    assert_eq!(out.status.code(), Some(2));
    let v = parse_stdout(&out);
    assert_eq!(v["report"]["passed"], false);
    assert!(all_incomplete(&v));
    assert!(is_null_field(&v, "metrics"));
    assert!(v["metrics_error"].as_str().is_some());

    // Valid range, but missing C4 records (Incomplete, no Fail) -> exits 2 with metrics present.
    let mut incomplete = Log::start("mn,dkg", RunKind::StageA, &[params(100)]);
    incomplete.blocks(H0 + 1..=HN);
    incomplete.push(commitment(100, 0, 120, 131, &[(0, true), (1, true), (2, true)]));
    incomplete.finish();
    let inc_plan = write_plan(&dir, "inc-plan.json", &incomplete.plan);
    let inc_obs = write_log(&dir, "inc.jsonl", &incomplete.jsonl());
    let out = run_judge(&inc_obs, &inc_plan, "stage-a");
    assert_eq!(out.status.code(), Some(2));
    let v = parse_stdout(&out);
    assert_eq!(v["report"]["passed"], false);
    assert_eq!(v["report"]["criteria"][3]["verdict"]["status"], "incomplete");
    assert_eq!(v["metrics"]["range"], json!([H0, HN]));
    assert!(is_null_field(&v, "metrics_error"));
}

#[test]
fn rehearsal_gate_passes_with_banner() {
    let dir = TempDir::new().unwrap();
    // A Stage-A log with a C2 Fail still passes the rehearsal gate because its range is complete.
    let mut log = Log::full();
    log.push(Record::MnDiff(MnDiff {
        height: 135,
        pro_tx: pro(reg(2)),
        pose_penalty: Some(66),
        pose_ban_height: None,
        pose_revived_height: None,
    }));
    let plan = write_plan(&dir, "plan.json", &log.plan);
    let obs = write_log(&dir, "obs.jsonl", &log.jsonl());

    let out = run_judge(&obs, &plan, "rehearsal");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let v = parse_stdout(&out);
    assert_eq!(v["report"]["gate"], "rehearsal");
    assert_eq!(v["report"]["passed"], true);
    assert_eq!(v["report"]["banner"], REHEARSAL_BANNER);
    assert_eq!(v["report"]["criteria"][1]["verdict"]["status"], "fail");
    assert_eq!(v["metrics"]["range"], json!([H0, HN]));
}

#[test]
fn malformed_line_exits_2_naming_line_number() {
    let dir = TempDir::new().unwrap();
    let log = Log::full();
    let plan = write_plan(&dir, "plan.json", &log.plan);
    let first = serde_json::to_string(&log.envelopes()[0]).unwrap();
    let text = format!("{first}\n\n{{not json\n");
    let obs = write_log(&dir, "bad.jsonl", &text);

    let out = run_judge(&obs, &plan, "stage-a");
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let stderr = String::from_utf8(out.stderr).unwrap();
    let serde = serde_json::from_str::<Envelope>("{not json").unwrap_err();
    assert_eq!(stderr, format!("mixnet-judge: line 3: {serde}\n"));
}

#[test]
fn plan_mismatch_exits_2() {
    let dir = TempDir::new().unwrap();
    let log = Log::full();
    let other_plan =
        RolePlan::new(Preset::ModeB5, ALL_CAPS.parse().unwrap(), Context::Regtest).unwrap();
    let plan = write_plan(&dir, "other-plan.json", &other_plan);
    let obs = write_log(&dir, "obs.jsonl", &log.jsonl());

    let out = run_judge(&obs, &plan, "stage-a");
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("plan.json does not match the log's run"), "stderr was: {stderr}");
}

#[test]
fn bad_arguments_exit_2_with_usage_and_empty_stdout() {
    let cases: &[(&[&str], &str)] = &[
        (&["obs.jsonl", "--gate", "stage-a"], "--plan is required"),
        (&["obs.jsonl", "--plan", "plan.json"], "--gate is required"),
        (&["--plan", "plan.json", "--gate", "stage-a"], "<obs.jsonl> is required"),
        (&["obs.jsonl", "--plan", "plan.json", "--gate", "stage-a", "--extra"], "unknown argument"),
        (
            &["obs.jsonl", "--plan", "a.json", "--plan", "b.json", "--gate", "stage-a"],
            "--plan is given twice",
        ),
        (
            &["obs.jsonl", "--plan", "a.json", "--gate", "stage-a", "--gate", "rehearsal"],
            "--gate is given twice",
        ),
        (
            &["a.jsonl", "b.jsonl", "--plan", "plan.json", "--gate", "stage-a"],
            "more than one log path",
        ),
        (&["obs.jsonl", "--plan"], "--plan needs a value"),
        (&["obs.jsonl", "--plan", "plan.json", "--gate"], "--gate needs a value"),
        (&["obs.jsonl", "--plan", "plan.json", "--gate", "soak"], "unknown gate"),
    ];
    for (args, expected) in cases {
        let out = run_bin(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
        let stderr = String::from_utf8(out.stderr).unwrap();
        assert!(stderr.contains(expected), "{args:?}: {stderr}");
        assert!(stderr.contains(USAGE), "{args:?}: {stderr}");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn failed_stdout_write_exits_2() {
    let dev_full = Path::new("/dev/full");
    if !dev_full.exists() {
        return;
    }
    let dir = TempDir::new().unwrap();
    let log = Log::full();
    let plan = write_plan(&dir, "plan.json", &log.plan);
    let obs = write_log(&dir, "obs.jsonl", &log.jsonl());
    for stderr_full in [false, true] {
        let stderr = if stderr_full {
            fs::File::create(dev_full).unwrap().into()
        } else {
            std::process::Stdio::null()
        };
        let status = Command::new(env!("CARGO_BIN_EXE_mixnet-judge"))
            .arg(&obs)
            .arg("--plan")
            .arg(&plan)
            .arg("--gate")
            .arg("stage-a")
            .stdout(fs::File::create(dev_full).unwrap())
            .stderr(stderr)
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(2), "stderr full: {stderr_full}");
    }
}

#[test]
fn a_flag_is_never_a_flag_value() {
    let dir = TempDir::new().unwrap();
    let log = Log::full();
    write_plan(&dir, "--plan", &log.plan);
    write_plan(&dir, "--gate", &log.plan);
    write_log(&dir, "obs.jsonl", "\n");
    for args in [
        &["obs.jsonl", "--plan", "--plan", "--gate", "stage-a"][..],
        &["obs.jsonl", "--plan", "--gate", "--gate", "stage-a"],
        &["obs.jsonl", "--gate", "--plan", "--plan", "--gate"],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_mixnet-judge"))
            .current_dir(dir.path())
            .args(args)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
        let stderr = String::from_utf8(out.stderr).unwrap();
        assert!(stderr.contains("needs a value"), "{args:?}: {stderr}");
        assert!(stderr.contains(USAGE), "{args:?}: {stderr}");
    }
    let out = Command::new(env!("CARGO_BIN_EXE_mixnet-judge"))
        .current_dir(dir.path())
        .args(["obs.jsonl", "--plan", "./--plan", "--gate", "rehearsal"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(all_incomplete(&parse_stdout(&out)));
}

#[cfg(unix)]
#[test]
fn a_non_utf8_dash_argument_is_an_unknown_flag() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let dir = TempDir::new().unwrap();
    let log = Log::full();
    write_plan(&dir, "plan.json", &log.plan);
    let name = OsStr::from_bytes(b"-\xff");
    fs::write(dir.path().join(name), "\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_mixnet-judge"))
        .current_dir(dir.path())
        .arg(name)
        .args(["--plan", "plan.json", "--gate", "rehearsal"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unknown argument"), "{stderr}");
    assert!(stderr.contains(USAGE), "{stderr}");
}

#[test]
fn a_non_utf8_line_gets_serdes_message() {
    let dir = TempDir::new().unwrap();
    let log = Log::full();
    let plan = write_plan(&dir, "plan.json", &log.plan);
    let mut bytes = b"\r\n{\"v\":\"".to_vec();
    bytes.extend_from_slice(b"\xff\"}\r\n");
    let obs = dir.path().join("raw.jsonl");
    fs::write(&obs, &bytes).unwrap();

    let out = run_judge(&obs, &plan, "stage-a");
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let serde = serde_json::from_slice::<Envelope>(&bytes[2..]).unwrap_err();
    assert_eq!(String::from_utf8(out.stderr).unwrap(), format!("mixnet-judge: line 2: {serde}\n"));
}

#[test]
fn crlf_lines_and_trailing_spaces_parse() {
    let dir = TempDir::new().unwrap();
    let log = Log::full();
    let plan = write_plan(&dir, "plan.json", &log.plan);
    let text = log.jsonl().replace('\n', " \t\r\n");
    let obs = write_log(&dir, "crlf.jsonl", &text);

    let out = run_judge(&obs, &plan, "stage-a");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
}

#[cfg(target_os = "linux")]
#[test]
fn a_failed_stderr_write_still_exits_2() {
    let dev_full = Path::new("/dev/full");
    if !dev_full.exists() {
        return;
    }
    for args in [&[][..], &["missing.jsonl", "--plan", "missing.json", "--gate", "stage-a"]] {
        let status = Command::new(env!("CARGO_BIN_EXE_mixnet-judge"))
            .args(args)
            .stderr(fs::File::create(dev_full).unwrap())
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(2), "{args:?}");
    }
}
