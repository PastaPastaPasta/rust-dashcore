//! O12b-1's contract: presets, the role rules over every caps × context, plan and record JSON.

use dash_testkit::mixnet::contract::{
    Binaries, Binary, Binding, Block, Cap, Caps, ChainTip, ChainTips, Chaos, CjSession, ClSeen,
    Clock, Commitment, Context, Envelope, Handoff, HandoffMethod, Hex32, Host, Impl, IsReq, IsSeen,
    LogError, Member, Miner, MnDiff, PlanError, Preset, QuorumRef, Record, Reorg, RolePlan, Run,
    RunKind, SlotId, Superblock, Tip, TipStatus, V1, Watermark, check_log,
};
use serde_json::{Value, json};

const CONTEXTS: [Context; 2] = [Context::Regtest, Context::Devnet];

fn presets() -> Vec<Preset> {
    [
        "modeb5",
        "alt5",
        "alt8",
        "allrust:5:3",
        "allrust:2:0",
        "allrust:0:1",
        "devnet",
        "ratio1:3",
        "ratio3:1",
    ]
    .iter()
    .map(|p| p.parse().unwrap())
    .collect()
}

fn every_caps() -> impl Iterator<Item = Caps> {
    (0u32..1 << Cap::ALL.len()).map(|bits| {
        Cap::ALL.iter().enumerate().filter(|(i, _)| bits & (1 << i) != 0).map(|(_, c)| *c).collect()
    })
}

fn caps(s: &str) -> Caps {
    s.parse().unwrap()
}

fn slots(list: &str) -> Vec<SlotId> {
    list.split_whitespace().map(|s| s.parse().unwrap()).collect()
}

fn range(prefix: &str, n: u16) -> String {
    (0..n).map(|i| format!("{prefix}{i} ")).collect()
}

fn h(byte: u8) -> Hex32 {
    format!("{byte:02x}").repeat(32).parse().unwrap()
}

#[test]
fn the_preset_table() {
    let table = [
        ("modeb5", Context::Regtest, (5, 0), range("reg", 5)),
        ("alt5", Context::Regtest, (5, 0), "reg0 reg2 reg4".into()),
        ("alt8", Context::Regtest, (8, 0), "reg0 reg2 reg4 reg6".into()),
        (
            "allrust:5:3",
            Context::Regtest,
            (5, 3),
            format!("node0 {}{}", range("reg", 5), range("evo", 3)),
        ),
        ("devnet", Context::Devnet, (20, 12), range("reg", 10) + &range("evo", 6)),
        ("ratio1:3", Context::Devnet, (20, 12), range("reg", 5) + &range("evo", 3)),
        ("ratio3:1", Context::Devnet, (20, 12), range("reg", 15) + &range("evo", 9)),
    ];
    for (name, context, counts, rust) in table {
        let preset: Preset = name.parse().unwrap();
        assert_eq!(preset.to_string(), name);
        assert_eq!((preset.context(), preset.counts()), (context, counts), "{name}");
        let plan = RolePlan::new(preset, caps("mn,dkg"), context).unwrap();
        assert_eq!(plan.slots.len(), 1 + usize::from(counts.0) + usize::from(counts.1), "{name}");
        let labelled: Vec<_> =
            plan.slots.iter().filter(|s| s.label == Impl::Rust).map(|s| s.slot).collect();
        assert_eq!(labelled, slots(&rust), "{name}");
        let order: Vec<_> = plan.slots.iter().map(|s| s.slot).collect();
        let expected = format!("node0 {}{}", range("reg", counts.0), range("evo", counts.1));
        assert_eq!(order, slots(&expected), "{name}");
    }
}

#[test]
fn every_caps_and_context_follows_the_role_rules() {
    let mut plans = 0;
    for preset in presets() {
        for caps in every_caps() {
            for context in CONTEXTS {
                let result = RolePlan::new(preset, caps.clone(), context);
                if context != preset.context() {
                    assert_eq!(
                        result,
                        Err(PlanError::Context {
                            preset,
                            context
                        })
                    );
                    continue;
                }
                let plan = result.unwrap();
                let (mn, dkg) = (caps.has(Cap::Mn), caps.has(Cap::Dkg));
                let mut rehearsal_only = false;
                for s in &plan.slots {
                    assert_eq!(s.label == Impl::Rust, preset.labels_rust(s.slot));
                    let runs_rust = match (s.slot, s.label) {
                        (_, Impl::Dashd) => false,
                        (SlotId::Node(_), Impl::Rust) => true,
                        (_, Impl::Rust) => mn && (dkg || context == Context::Regtest),
                    };
                    assert_eq!(
                        s.runs == Impl::Rust,
                        runs_rust,
                        "{preset} {caps} {context} {}",
                        s.slot
                    );
                    assert_eq!(
                        s.twin,
                        runs_rust && s.slot.is_masternode() && !dkg,
                        "{preset} {caps} {}",
                        s.slot
                    );
                    rehearsal_only |= s.label == Impl::Rust && !runs_rust;
                    rehearsal_only |= s.slot == SlotId::Node(0) && runs_rust;
                }
                let kinds = match (context, rehearsal_only) {
                    (Context::Regtest, true) => vec![RunKind::Rehearsal],
                    (Context::Regtest, false) => vec![RunKind::Rehearsal, RunKind::StageA],
                    (Context::Devnet, true) => vec![RunKind::DevnetRehearsal],
                    (Context::Devnet, false) => vec![RunKind::DevnetRehearsal, RunKind::Soak],
                };
                assert_eq!(plan.run_kinds, kinds, "{preset} {caps} {context}");
                for kind in RunKind::ALL {
                    assert_eq!(plan.admits(kind), kinds.contains(&kind));
                }
                let json = serde_json::to_string(&plan).unwrap();
                assert_eq!(serde_json::from_str::<RolePlan>(&json).unwrap(), plan);
                assert_eq!(caps.to_string().parse::<Caps>().unwrap(), caps);
                plans += 1;
            }
        }
    }
    assert_eq!(plans, presets().len() * 256);
}

#[test]
fn the_rules_on_the_plans_runs_use() {
    let plan =
        |p: &str, c: &str, x: Context| RolePlan::new(p.parse().unwrap(), caps(c), x).unwrap();
    let modeb = plan("modeb5", "mn", Context::Regtest);
    assert!(modeb.slots[1..].iter().all(|s| s.runs == Impl::Rust && s.twin));
    assert_eq!(modeb.slot(SlotId::CONTROLLER).unwrap().runs, Impl::Dashd);
    assert!(modeb.admits(RunKind::StageA));

    let real = plan("modeb5", "mn,dkg", Context::Regtest);
    assert!(real.slots[1..].iter().all(|s| s.runs == Impl::Rust && !s.twin));

    let alt = plan("alt5", "none", Context::Regtest);
    assert!(alt.slots.iter().all(|s| s.runs == Impl::Dashd && !s.twin));
    assert_eq!(alt.run_kinds, [RunKind::Rehearsal]);

    let devnet = plan("devnet", "mn", Context::Devnet);
    assert!(devnet.slots.iter().all(|s| s.runs == Impl::Dashd));
    assert_eq!(devnet.run_kinds, [RunKind::DevnetRehearsal]);
    let devnet = plan("devnet", "mn,dkg", Context::Devnet);
    assert_eq!(devnet.slots.iter().filter(|s| s.runs == Impl::Rust && !s.twin).count(), 16);
    assert!(devnet.admits(RunKind::Soak));

    let allrust = plan("allrust:5:3", "none", Context::Regtest);
    let node0 = allrust.slot(SlotId::CONTROLLER).unwrap();
    assert_eq!((node0.label, node0.runs, node0.twin), (Impl::Rust, Impl::Rust, false));
    assert!(allrust.slots[1..].iter().all(|s| s.label == Impl::Rust && s.runs == Impl::Dashd));
    assert_eq!(allrust.run_kinds, [RunKind::Rehearsal]);
    let allrust = plan("allrust:5:3", "mn,sig,lock,qdata,gbt,dkg", Context::Regtest);
    assert!(allrust.slots.iter().all(|s| s.runs == Impl::Rust));
    assert_eq!(allrust.run_kinds, [RunKind::Rehearsal]);
}

#[test]
fn the_example_plans_are_the_clis_output() {
    let examples = [
        (include_str!("../mixnet/plan-regtest.json"), "modeb5", "mn", Context::Regtest),
        (include_str!("../mixnet/plan-rehearsal.json"), "alt5", "none", Context::Regtest),
        (include_str!("../mixnet/plan-devnet.json"), "devnet", "mn,dkg", Context::Devnet),
    ];
    for (file, preset, c, context) in examples {
        let plan = RolePlan::new(preset.parse().unwrap(), caps(c), context).unwrap();
        assert_eq!(file, serde_json::to_string_pretty(&plan).unwrap() + "\n", "{preset}");
        assert_eq!(serde_json::from_str::<RolePlan>(file).unwrap(), plan);
    }
}

#[test]
fn a_plan_file_must_be_the_rules() {
    let plan = RolePlan::new(Preset::Alt5, caps("mn"), Context::Regtest).unwrap();
    let good = serde_json::to_value(&plan).unwrap();
    assert_eq!(serde_json::from_value::<RolePlan>(good.clone()).unwrap(), plan);
    let edit = |f: &dyn Fn(&mut Value)| {
        let mut v = good.clone();
        f(&mut v);
        serde_json::from_value::<RolePlan>(v).unwrap_err().to_string()
    };
    assert!(edit(&|v| v["slots"][2]["runs"] = json!("rust")).contains("not the rules"));
    assert!(edit(&|v| v["slots"][1]["twin"] = json!(false)).contains("not the rules"));
    assert!(edit(&|v| v["run_kinds"] = json!(["rehearsal"])).contains("not the rules"));
    assert!(edit(&|v| drop(v["slots"].as_array_mut().unwrap().pop())).contains("not the rules"));
    assert!(edit(&|v| v["context"] = json!("devnet")).contains("is a regtest preset"));
    assert!(edit(&|v| v["v"] = json!(2)).contains("unsupported version 2"));
    assert!(edit(&|v| v["extra"] = json!(1)).contains("unknown field"));
    assert!(edit(&|v| v["slots"][0]["extra"] = json!(1)).contains("unknown field"));
    assert!(edit(&|v| v["caps"] = json!(["mn", "mn"])).contains("twice"));
    assert!(edit(&|v| v["caps"] = json!(["mn", "probe"])).contains("unknown variant"));
    assert!(edit(&|v| v["preset"] = json!("alt6")).contains("not a valid preset"));
}

#[test]
fn text_forms_are_strict() {
    assert_eq!(caps("none"), Caps::none());
    assert_eq!(caps("dkg,mn").to_string(), "mn,dkg");
    assert_eq!(Caps::none().to_string(), "none");
    for bad in ["", "mn,", ",mn", "mn,mn", "MN", "mn sig", "none,mn", "probe"] {
        assert!(bad.parse::<Caps>().is_err(), "{bad:?}");
    }
    for good in
        ["modeb5", "alt5", "alt8", "allrust:1:0", "allrust:0:12", "devnet", "ratio1:3", "ratio3:1"]
    {
        assert_eq!(good.parse::<Preset>().unwrap().to_string(), good);
    }
    for bad in [
        "allrust",
        "allrust:5",
        "allrust:0:0",
        "allrust:05:3",
        "allrust:+5:3",
        "allrust:5:3:1",
        "ratio1:2",
    ] {
        assert!(bad.parse::<Preset>().is_err(), "{bad:?}");
    }
    for good in ["node0", "node3", "reg0", "reg19", "evo11"] {
        assert_eq!(good.parse::<SlotId>().unwrap().to_string(), good);
    }
    for bad in ["node", "reg01", "reg-1", "evo+1", "mn0", "Reg0", "reg65536"] {
        assert!(bad.parse::<SlotId>().is_err(), "{bad:?}");
    }
    assert_eq!("regtest".parse::<Context>().unwrap(), Context::Regtest);
    assert!("mainnet".parse::<Context>().is_err());
    let hex = h(0xab).to_string();
    assert!(hex.to_uppercase().parse::<Hex32>().is_err());
    assert!(hex[1..].parse::<Hex32>().is_err());
    assert!(format!("{hex}0").parse::<Hex32>().is_err());
    assert!(format!("{}g", &hex[1..]).parse::<Hex32>().is_err());
}

fn one_of_each_record() -> Vec<Record> {
    let binary = |b| Binary {
        sha256: h(b),
        version: "v24.0.2".into(),
    };
    vec![
        Record::Run(Run {
            run_kind: RunKind::StageA,
            context: Context::Regtest,
            preset: Preset::ModeB5,
            caps: caps("mn,sig"),
            host: Host {
                nproc: 32,
                mem: 1 << 37,
                disk: 1 << 40,
            },
            clock: Clock {
                source: "chrony".into(),
                offset_ms: -3,
            },
            binaries: Binaries {
                dashd: binary(1),
                rdashd: Some(binary(2)),
            },
        }),
        Record::Binding(Binding {
            slot: SlotId::Regular(0),
            pro_tx: h(3),
            label: Impl::Rust,
            runs: Impl::Rust,
            exe_sha256: h(2),
            from_height: 210,
            from_seq: 4,
        }),
        Record::Block(Block {
            height: 211,
            hash: h(4),
            prev: h(5),
            miner: Some(Miner {
                slot: SlotId::CONTROLLER,
                runs: Impl::Dashd,
            }),
        }),
        Record::Block(Block {
            height: 200,
            hash: h(5),
            prev: h(6),
            miner: None,
        }),
        Record::Reorg(Reorg {
            fork_height: 209,
            old_tip: h(4),
            new_tip: h(7),
        }),
        Record::Watermark(Watermark {
            height: 211,
        }),
        Record::Tip(Tip {
            node: SlotId::Evo(2),
            height: 211,
            hash: h(4),
        }),
        Record::ChainTips(ChainTips {
            node: SlotId::Regular(1),
            tips: vec![
                ChainTip {
                    height: 211,
                    hash: h(4),
                    branchlen: 0,
                    status: TipStatus::Active,
                },
                ChainTip {
                    height: 210,
                    hash: h(7),
                    branchlen: 1,
                    status: TipStatus::ValidFork,
                },
            ],
        }),
        Record::Commitment(Commitment {
            llmq_type: 100,
            quorum_hash: h(8),
            index: 0,
            base_height: 216,
            mined_height: 228,
            null: false,
            members: vec![
                Member {
                    pro_tx: h(3),
                    valid: true,
                },
                Member {
                    pro_tx: h(9),
                    valid: false,
                },
            ],
            vvec_hash: h(10),
        }),
        Record::MnDiff(MnDiff {
            height: 229,
            pro_tx: h(9),
            pose_penalty: Some(66),
            pose_ban_height: None,
            pose_revived_height: None,
        }),
        Record::Handoff(Handoff {
            slot: SlotId::Regular(0),
            quorum: QuorumRef {
                llmq_type: 100,
                quorum_hash: h(8),
            },
            method: HandoffMethod::TwinRetained,
            source_slots: vec![],
            ok: true,
        }),
        Record::ClSeen(ClSeen {
            node: SlotId::CONTROLLER,
            height: 228,
            hash: h(11),
        }),
        Record::IsReq(IsReq {
            txid: h(12),
        }),
        Record::IsSeen(IsSeen {
            node: SlotId::Node(1),
            txid: h(12),
        }),
        Record::CjSession(CjSession {
            wallet: "w0".into(),
            coordinator_pro_tx: h(3),
            outcome: "success".into(),
        }),
        Record::Superblock(Superblock {
            height: 240,
            trigger_hash: h(13),
            creator_pro_tx: h(9),
            paid: true,
        }),
        Record::Chaos(Chaos {
            id: "c1".into(),
            action: "stop".into(),
            targets: vec![SlotId::Regular(1), SlotId::Evo(0)],
            start_seq: 9,
            end_seq: 12,
        }),
        Record::MnDiff(MnDiff {
            height: 230,
            pro_tx: h(9),
            pose_penalty: Some(0),
            pose_ban_height: Some(-1),
            pose_revived_height: Some(230),
        }),
    ]
}

fn log() -> Vec<Envelope> {
    one_of_each_record()
        .into_iter()
        .enumerate()
        .map(|(i, record)| Envelope {
            v: V1,
            run: "r1".into(),
            seq: 10 * i as u64 + 1,
            mono_ns: 1_000 * i as u64,
            src: "watch".into(),
            record,
        })
        .collect()
}

#[test]
fn every_record_kind_round_trips_flat() {
    let kinds = [
        "Run",
        "Binding",
        "Block",
        "Block",
        "Reorg",
        "Watermark",
        "Tip",
        "ChainTips",
        "Commitment",
        "MnDiff",
        "Handoff",
        "ClSeen",
        "IsReq",
        "IsSeen",
        "CjSession",
        "Superblock",
        "Chaos",
        "MnDiff",
    ];
    let log = log();
    assert_eq!(log.len(), kinds.len());
    for (e, kind) in log.iter().zip(kinds) {
        let line = serde_json::to_string(e).unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["v"], 1);
        assert_eq!(v["kind"], kind);
        assert_eq!((&v["run"], &v["src"]), (&json!("r1"), &json!("watch")));
        assert!(v.get("record").is_none());
        assert_eq!(serde_json::from_str::<Envelope>(&line).unwrap(), *e, "{kind}");
    }
    let commitment: Value = serde_json::to_value(&log[8]).unwrap();
    assert_eq!(commitment["type"], 100);
    assert_eq!(serde_json::to_value(&log[7]).unwrap()["tips"][1]["status"], "valid-fork");
    assert_eq!(serde_json::to_value(&log[10]).unwrap()["method"], "twin-retained");
    assert_eq!(serde_json::to_value(&log[0]).unwrap()["run_kind"], "stage-a");
    assert_eq!(serde_json::to_value(&log[0]).unwrap()["caps"], json!(["mn", "sig"]));
}

#[test]
fn a_record_line_must_be_known() {
    let good = serde_json::to_value(&log()[5]).unwrap();
    let read = |f: &dyn Fn(&mut Value)| {
        let mut v = good.clone();
        f(&mut v);
        serde_json::from_value::<Envelope>(v).map_err(|e| e.to_string())
    };
    assert!(read(&|_| ()).is_ok());
    assert!(read(&|v| v["v"] = json!(2)).unwrap_err().contains("unsupported version 2"));
    assert!(read(&|v| v["v"] = json!("1")).is_err());
    assert!(read(&|v| v["kind"] = json!("ProbeRecovery")).unwrap_err().contains("unknown variant"));
    assert!(read(&|v| v["kind"] = json!("watermark")).unwrap_err().contains("unknown variant"));
    assert!(read(&|v| drop(v.as_object_mut().unwrap().remove("kind"))).is_err());
    assert!(read(&|v| drop(v.as_object_mut().unwrap().remove("seq"))).is_err());
    assert!(read(&|v| drop(v.as_object_mut().unwrap().remove("height"))).is_err());
    assert!(read(&|v| v["height"] = json!(-1)).is_err());
    assert!(read(&|v| v["extra"] = json!(1)).unwrap_err().contains("unknown field"));
    let tip = serde_json::to_value(&log()[6]).unwrap();
    let mut bad = tip.clone();
    bad["hash"] = json!(h(0xab).to_string().to_uppercase());
    assert!(serde_json::from_value::<Envelope>(bad).is_err());
    let mut bad = tip;
    bad["node"] = json!("reg01");
    assert!(serde_json::from_value::<Envelope>(bad).is_err());
    let mut chain_tips = serde_json::to_value(&log()[7]).unwrap();
    chain_tips["tips"][0]["status"] = json!("unknown");
    assert!(serde_json::from_value::<Envelope>(chain_tips.clone()).is_ok());
    chain_tips["tips"][0]["status"] = json!("stale");
    assert!(serde_json::from_value::<Envelope>(chain_tips).is_err());
}

#[test]
fn a_log_is_one_admitted_run_in_seq_order() {
    let log = log();
    assert_eq!(check_log(&log), Ok(()));
    assert_eq!(check_log(&log[..1]), Ok(()));
    assert_eq!(check_log(&[]), Err(LogError::Empty));
    assert_eq!(check_log(&log[1..]), Err(LogError::Run(0)));
    let mut twice = log.clone();
    twice[3].record = log[0].record.clone();
    assert_eq!(check_log(&twice), Err(LogError::Run(3)));
    let mut other = log.clone();
    other[4].run = "r2".into();
    assert_eq!(check_log(&other), Err(LogError::RunId(4)));
    let mut same = log.clone();
    same[5].seq = same[4].seq;
    assert_eq!(check_log(&same), Err(LogError::Seq(5)));
    let mut back = log.clone();
    back[2].seq = 0;
    assert_eq!(check_log(&back), Err(LogError::Seq(2)));
    let with_run = |f: &dyn Fn(&mut Run)| {
        let mut log = log.clone();
        let Record::Run(run) = &mut log[0].record else {
            unreachable!()
        };
        f(run);
        check_log(&log)
    };
    assert_eq!(
        with_run(&|r| r.context = Context::Devnet),
        Err(LogError::Plan(PlanError::Context {
            preset: Preset::ModeB5,
            context: Context::Devnet
        }))
    );
    assert_eq!(with_run(&|r| r.run_kind = RunKind::Soak), Err(LogError::RunKind(RunKind::Soak)));
    assert_eq!(with_run(&|r| r.caps = caps("sig")), Err(LogError::RunKind(RunKind::StageA)));
    assert!(with_run(&|r| r.caps = caps("sig")).unwrap_err().to_string().contains("StageA"));
    assert_eq!(with_run(&|r| r.run_kind = RunKind::Rehearsal), Ok(()));
    assert_eq!(with_run(&|r| r.binaries.rdashd = None), Err(LogError::NoRdashd));
    assert_eq!(
        with_run(&|r| {
            r.binaries.rdashd = None;
            r.run_kind = RunKind::Rehearsal;
            r.caps = Caps::none();
        }),
        Ok(())
    );
    assert_eq!(
        with_run(&|r| r.preset = Preset::AllRust {
            regular: 5,
            evo: 0
        }),
        Err(LogError::RunKind(RunKind::StageA))
    );
}

/// The JSON pointer of every object in `v`, `v` itself included.
fn objects(v: &Value, at: String, out: &mut Vec<String>) {
    match v {
        Value::Object(map) => {
            out.push(at.clone());
            for (k, child) in map {
                objects(child, format!("{at}/{k}"), out);
            }
        }
        Value::Array(items) => {
            for (i, child) in items.iter().enumerate() {
                objects(child, format!("{at}/{i}"), out);
            }
        }
        _ => {}
    }
}

#[test]
fn every_record_object_refuses_an_unknown_field() {
    let mut checked = 0;
    for e in log() {
        let good = serde_json::to_value(&e).unwrap();
        let mut pointers = Vec::new();
        objects(&good, String::new(), &mut pointers);
        for at in pointers {
            let mut bad = good.clone();
            bad.pointer_mut(&at).unwrap().as_object_mut().unwrap().insert("extra".into(), json!(1));
            let err = serde_json::from_value::<Envelope>(bad).unwrap_err().to_string();
            assert!(err.contains("unknown field"), "{at:?} in {}: {err}", good["kind"]);
            checked += 1;
        }
    }
    assert_eq!(checked, 18 + 11);
}

#[test]
fn a_nullable_field_must_be_present() {
    let log = log();
    let drop_key = |e: &Envelope, at: &str, key: &str| {
        let mut v = serde_json::to_value(e).unwrap();
        v.pointer_mut(at).unwrap().as_object_mut().unwrap().remove(key).unwrap();
        serde_json::from_value::<Envelope>(v).map_err(|e| e.to_string())
    };
    for (e, at, key) in [
        (&log[0], "/binaries", "rdashd"),
        (&log[2], "", "miner"),
        (&log[3], "", "miner"),
        (&log[9], "", "pose_penalty"),
        (&log[9], "", "pose_ban_height"),
        (&log[9], "", "pose_revived_height"),
    ] {
        assert!(drop_key(e, at, key).unwrap_err().contains("missing field"), "{key}");
    }
    let mut half = serde_json::to_value(&log[2]).unwrap();
    half["miner"].as_object_mut().unwrap().remove("runs");
    assert!(serde_json::from_value::<Envelope>(half).is_err());
    let mut core_only = serde_json::to_value(&log[0]).unwrap();
    core_only["binaries"]["rdashd"] = Value::Null;
    let Record::Run(run) = serde_json::from_value::<Envelope>(core_only).unwrap().record else {
        unreachable!()
    };
    assert_eq!(run.binaries.rdashd, None);
    let mut empty = serde_json::to_value(&log[9]).unwrap();
    empty["pose_penalty"] = Value::Null;
    let err = serde_json::from_value::<Envelope>(empty).unwrap_err().to_string();
    assert!(err.contains("at least one PoSe field"), "{err}");
    let revival = serde_json::to_value(&log[17]).unwrap();
    assert_eq!(
        (&revival["pose_penalty"], &revival["pose_ban_height"], &revival["pose_revived_height"]),
        (&json!(0), &json!(-1), &json!(230))
    );
}

#[test]
fn the_cli_prints_the_plan_or_exits_2() {
    let run = |args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_mixnet-plan")).args(args).output().unwrap()
    };
    let out = run(&["--context", "devnet", "--caps", "mn,dkg", "--preset", "devnet"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8(out.stdout).unwrap(), include_str!("../mixnet/plan-devnet.json"));
    let refused = [
        (&["--preset", "devnet", "--caps", "mn", "--context", "regtest"][..], "is a devnet preset"),
        (&["--preset", "alt5", "--caps", "mn"], "--context is required"),
        (
            &["--preset", "alt5", "--caps", "mn", "--caps", "sig", "--context", "regtest"],
            "given twice",
        ),
        (
            &["--preset", "alt5", "--caps", "mn,probe", "--context", "regtest"],
            "not a valid cap list",
        ),
        (&["--preset", "alt6", "--caps", "mn", "--context", "regtest"], "not a valid preset"),
        (&["--preset", "alt5", "--caps", "mn", "--context", "mainnet"], "not a valid context"),
        (&["--preset"], "--preset needs a value"),
        (&["--plan"], "unknown argument"),
    ];
    for (args, message) in refused {
        let out = run(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
        assert!(String::from_utf8(out.stderr).unwrap().contains(message), "{args:?}");
    }
}
