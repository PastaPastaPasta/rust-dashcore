//! LK1b-s S1e: the direct-ingestion differential (PLAN-LK1b §6 S1e). The fixtures
//! `tests/data/lk1b-is/{main,rot}.json` are one Core recorder's run each (`contrib/node/lk1b-captures`):
//! every op it took, the O8 records it wrote while the op ran, and the raw IS DB rows at each
//! restart. The end-to-end test drives NodeSut (with `RecordsTap`) through the same ops and
//! compares each one (`lk1b_is::compare_step`); it needs the `test-support` feature. The loader
//! and the comparison are tested here on the hand-made `sample.json`, and the captures are checked
//! to load and to hold what their scenarios were captured for.

#[allow(dead_code)]
mod lk1b_is;

use dash_statedigest::{Hash256, Record, parse_line};
use lk1b_is::{
    Fixture, IsSide, Mismatch, Observed, Op, RestartKind, Rows, Step, sample, sample_text,
};

fn encode(records: &[Record]) -> String {
    let mut out = Vec::new();
    records.iter().for_each(|r| r.encode(&mut out));
    String::from_utf8(out).expect("utf-8")
}

fn parse(text: &str) -> Vec<Record> {
    text.split_inclusive('\n')
        .map(|line| parse_line(line.as_bytes()).expect("rewritten line parses"))
        .collect()
}

/// The same records after a textual edit of their canonical lines.
fn rewrite(records: &[Record], from: &str, to: &str) -> Vec<Record> {
    let text = encode(records);
    assert!(text.contains(from), "rewrite {from:?}");
    parse(&text.replace(from, to))
}

/// A Rust side that answers each op with the records and rows a closure gives for its step.
struct Scripted<F: FnMut(&Step) -> Observed> {
    steps: Vec<Step>,
    next: usize,
    times: Vec<i64>,
    answer: F,
}

impl<F: FnMut(&Step) -> Observed> IsSide for Scripted<F> {
    fn set_time(&mut self, mocktime: i64) {
        self.times.push(mocktime);
    }

    fn apply(&mut self, op: &Op) -> Result<Observed, String> {
        let step = self.steps.get(self.next).ok_or("more ops than steps")?.clone();
        self.next += 1;
        assert_eq!(&step.op, op, "ops arrive in step order");
        Ok((self.answer)(&step))
    }
}

/// What Core's recorder wrote for a step, as the Rust side would report it: on the kill-dependent
/// step the KILL checkpoint's rows and the re-delivered block as the tip.
fn echo(step: &Step) -> Observed {
    let tip = match (&step.kill_dependent, &step.op) {
        (
            Some(_),
            Op::Block {
                hash,
                ..
            },
        ) => Some(*hash),
        _ => None,
    };
    Observed {
        records: step.records.clone(),
        rows: step.checkpoint.as_ref().or(step.kill_dependent.as_ref()).map(|c| c.rows.clone()),
        tip,
    }
}

/// [`echo`], with step `n`'s records edited.
fn edit_step(n: usize, edit: impl Fn(Vec<Record>) -> Vec<Record>) -> impl FnMut(&Step) -> Observed {
    move |s| {
        let mut o = echo(s);
        if s.step == n {
            o.records = edit(o.records);
        }
        o
    }
}

fn drive_with(f: impl FnMut(&Step) -> Observed) -> Vec<Mismatch> {
    drive_fixture(&sample(), f)
}

fn drive_fixture(fixture: &Fixture, f: impl FnMut(&Step) -> Observed) -> Vec<Mismatch> {
    let mut side = Scripted {
        steps: fixture.steps.clone(),
        next: 0,
        times: Vec::new(),
        answer: f,
    };
    let out = lk1b_is::drive(fixture, &mut side).expect("drives");
    assert_eq!(side.next, fixture.steps.len());
    // Each step's first A record's t, else its mocktime, carried forward.
    let mut times: Vec<i64> = Vec::new();
    for s in &fixture.steps {
        let t = first_a_time(s).unwrap_or(s.mocktime);
        times.push(times.last().map_or(t, |&prev| prev.max(t)));
    }
    assert_eq!(side.times, times);
    assert!(side.times.is_sorted(), "the clock runs backwards: {:?}", side.times);
    out
}

fn first_a_time(s: &Step) -> Option<i64> {
    s.records.iter().find_map(|r| match r {
        Record::Attempt(a) => Some(a.t),
        _ => None,
    })
}

#[test]
fn the_clock_is_the_first_attempts_time() {
    let mut side = Scripted {
        steps: sample().steps,
        next: 0,
        times: Vec::new(),
        answer: echo,
    };
    lk1b_is::drive(&sample(), &mut side).unwrap();
    // Steps 3 and 4 have A records (t 1700000100 and 1700000101); the others their mocktime,
    // except step 5, whose stale mocktime (1700000005) would set the clock back.
    assert_eq!(side.times, [1700000001, 1700000002, 1700000100, 1700000101, 1700000101]);
}

#[test]
fn the_clock_never_runs_backwards() {
    // Stale fallback mocktimes after a later A time, and one A time earlier than the clock.
    let mut f = sample();
    f.steps[0].mocktime = 1700000050;
    f.steps[1].mocktime = 1700000040;
    f.steps[3].records = rewrite(&f.steps[3].records, "\"t\":1700000101", "\"t\":1700000099");
    f.steps[4].mocktime = 1700000200;
    let mut side = Scripted {
        steps: f.steps.clone(),
        next: 0,
        times: Vec::new(),
        answer: echo,
    };
    lk1b_is::drive(&f, &mut side).unwrap();
    assert_eq!(side.times, [1700000050, 1700000050, 1700000100, 1700000100, 1700000200]);
    // The captures: without carrying the clock forward, 7 main and 4 rot steps would set it back.
    for (run, rewinds) in [("main", 7), ("rot", 4)] {
        let f = lk1b_is::capture(run);
        let raw: Vec<_> = f.steps.iter().map(|s| first_a_time(s).unwrap_or(s.mocktime)).collect();
        assert_eq!(raw.windows(2).filter(|w| w[1] < w[0]).count(), rewinds, "{run}");
        let mut side = Scripted {
            steps: f.steps.clone(),
            next: 0,
            times: Vec::new(),
            answer: echo,
        };
        lk1b_is::drive(&f, &mut side).unwrap();
        assert!(side.times.is_sorted(), "{run}");
    }
}

#[test]
fn sample_fixture_loads() {
    let f = sample();
    assert_eq!(f.run, "sample");
    let tags: Vec<_> = f.steps.iter().map(|s| s.op.tag()).collect();
    assert_eq!(tags, ["SPORK", "ISLOCK", "BLOCK", "CL_RECORD", "RESTART"]);
    assert!(matches!(
        f.steps[0].op,
        Op::Spork {
            id: 10001,
            value: 0
        }
    ));
    let Op::IsLock {
        mined_height,
        ..
    } = &f.steps[1].op
    else {
        unreachable!()
    };
    assert_eq!(*mined_height, None);
    assert!(matches!(
        &f.steps[4].op,
        Op::Restart {
            kind: RestartKind::Clean,
            args
        } if args.is_empty()
    ));
    assert_eq!(f.steps[2].conflict, f.steps[3].conflict);
    assert_eq!(f.steps[2].conflict.len(), 1);
    // Step 3 is the R conflict-tx-lock with its A; step 4 the ChainLocked C with `is_rm`.
    let a3 = lk1b_is::attempts(&f.steps[2].records);
    assert_eq!(a3.len(), 1);
    assert_eq!(a3[0].fx, Some(vec![]));
    let a4 = lk1b_is::attempts(&f.steps[3].records);
    assert!(matches!(a4[0].fx.as_deref(), Some([dash_statedigest::Effect::IsRm { .. }])));
    let q = lk1b_is::is_conflict_queries(&f.steps[2].records);
    assert_eq!(q.len(), 1);
    assert_eq!(q[0].1.len(), 1);
    let ckpt = f.steps[4].checkpoint.as_ref().expect("checkpoint");
    assert_eq!(ckpt.rows.len(), 3);
    assert!(f.steps[..4].iter().all(|s| s.checkpoint.is_none()));
}

/// The sample with one edit of its JSON, which the loader must refuse with a message containing
/// `want`.
fn refused(edit: impl FnOnce(&mut serde_json::Value), want: &str) {
    let mut f: serde_json::Value = serde_json::from_str(&sample_text()).unwrap();
    edit(&mut f);
    let e = lk1b_is::load(&f.to_string()).expect_err(want);
    assert!(e.contains(want), "want {want:?}: {e}");
}

/// A digest or decision line with one textual edit; the edited line must still be JSON.
fn edit_line(f: &mut serde_json::Value, step: usize, list: &str, from: &str, to: &str) {
    let line = &mut f["steps"][step][list][0];
    let text = line.as_str().unwrap();
    assert!(text.contains(from), "{from:?} in {text}");
    *line = serde_json::json!(text.replacen(from, to, 1));
}

#[test]
fn loader_refuses_malformed_fixtures() {
    refused(|f| f["schema"] = serde_json::json!("lk1b-is/2"), "schema");
    refused(|f| f["steps"][2]["step"] = serde_json::json!(7), "numbered 7");
    refused(|f| f["steps"][0]["op"]["op"] = serde_json::json!("SETTIME"), "unknown op");
    refused(|f| f["steps"][4]["op"]["kind"] = serde_json::json!("HALT"), "unknown restart kind");
    // A non-canonical O8 line (a space after a colon).
    refused(
        |f| edit_line(f, 2, "digest", "\"res\":", "\"res\": "),
        "step 3: digest[0]: ParseError",
    );
    // An A line among the digest lines.
    refused(
        |f| {
            let a = f["steps"][2]["decisions"][0].clone();
            f["steps"][2]["digest"] = serde_json::json!([a]);
        },
        "among the digest lines",
    );
    // A conflict block without an attempt record in its step.
    refused(|f| f["steps"][1]["conflict"] = f["steps"][2]["conflict"].clone(), "has no record");
    refused(|f| f["steps"][1]["op"]["inv_hash"] = serde_json::json!("00".repeat(32)), "sha256d");
    refused(
        |f| f["steps"][1]["op"]["txid"] = serde_json::json!("00".repeat(32)),
        "txid is not the payload's",
    );
    refused(
        |f| f["steps"][2]["periodic"] = serde_json::json!(["{\"k\": \"R\"}"]),
        "step 3: periodic[0]",
    );
    // A canonical periodic line outside scenario (d)'s retries.
    refused(
        |f| f["steps"][2]["periodic"] = f["steps"][2]["digest"].clone(),
        "periodic record is not a retry of (d)'s ChainLocked block",
    );
    // A block whose hash is not the X11 of its header.
    refused(
        |f| f["steps"][2]["op"]["hash"] = serde_json::json!("00".repeat(32)),
        "step 3 op: hash is not the X11 of the header",
    );
    refused(
        |f| {
            let bytes = f["steps"][2]["op"]["bytes"].as_str().unwrap().replacen("00", "01", 1);
            f["steps"][2]["op"]["bytes"] = serde_json::json!(bytes);
        },
        "step 3 op: hash is not the X11 of the header",
    );
    // A CL_RECORD whose inv hash, height, hash or signature is not its clsig payload's.
    let not_payload = "step 4 op: inv_hash is not sha256d of the clsig payload";
    refused(|f| f["steps"][3]["op"]["inv_hash"] = serde_json::json!("00".repeat(32)), not_payload);
    refused(|f| f["steps"][3]["op"]["height"] = serde_json::json!(106), not_payload);
    refused(|f| f["steps"][3]["op"]["hash"] = serde_json::json!("00".repeat(32)), not_payload);
    refused(|f| f["steps"][3]["op"]["sig"] = serde_json::json!("67".repeat(96)), not_payload);
    // Checkpoints on a step other than a RESTART, a KILL or the last, and a restart without one.
    refused(
        |f| f["steps"][2]["checkpoint"] = f["steps"][4]["checkpoint"].clone(),
        "step 3: a checkpoint on a BLOCK step",
    );
    refused(
        |f| f["steps"][4]["checkpoint"] = serde_json::Value::Null,
        "step 5: a RESTART without a checkpoint",
    );
    refused(
        |f| {
            f["steps"][4]["op"]["kind"] = serde_json::json!("KILL");
            f["steps"][4]["checkpoint"] = serde_json::Value::Null;
        },
        "step 5: a RESTART without a checkpoint",
    );
    // A checkpoint on the last step is allowed whatever its op.
    let mut f: serde_json::Value = serde_json::from_str(&sample_text()).unwrap();
    let mut last = f["steps"][0].clone();
    last["step"] = serde_json::json!(6);
    last["checkpoint"] = f["steps"][4]["checkpoint"].clone();
    f["steps"].as_array_mut().unwrap().push(last);
    lk1b_is::load(&f.to_string()).expect("a checkpoint on the last step");
    // An unknown scenario name, and a capture without one of its scenarios.
    refused(
        |f| f["steps"][2]["scenario"] = serde_json::json!("i"),
        "step 3: unknown scenario \"i\"",
    );
    refused(|f| f["run"] = serde_json::json!("rot"), "fixture rot: no step of scenario (rot)");
    refused(|f| f["run"] = serde_json::json!("main"), "fixture main: no step of scenario (a)");
    // A checkpoint whose digest does not match its rows, a row under the wrong table, rows out of
    // order.
    refused(
        |f| f["steps"][4]["checkpoint"]["is_locks"] = serde_json::json!("11".repeat(32)),
        "does not match its rows",
    );
    refused(
        |f| f["steps"][4]["checkpoint"]["rows"][0][2] = serde_json::json!("is_m"),
        "names table",
    );
    refused(
        |f| f["steps"][4]["checkpoint"]["rows"].as_array_mut().unwrap().swap(0, 1),
        "key order",
    );
}

#[test]
fn is_locks_digest_covers_the_lock_tables_only() {
    let lock: &[u8] = b"\x04is_i\x01";
    let mined: &[u8] = b"\x04is_m\x02";
    let a1: &[u8] = b"\x05is_a1\x03";
    let a2: &[u8] = b"\x05is_a2\x04";
    let base = lk1b_is::is_locks_digest([lock, mined, a1, a2]);
    assert_eq!(base, lk1b_is::is_locks_digest([a2, a1, mined, lock]), "order-free");
    let noise: [&[u8]; 4] = [b"\x04is_v", b"\x05is_tx\x05", b"\x05is_in\x06", b"zz"];
    let mut with_noise = vec![lock, mined, a1, a2];
    with_noise.extend(noise);
    assert_eq!(base, lk1b_is::is_locks_digest(with_noise));
    assert_ne!(base, lk1b_is::is_locks_digest([lock, mined, a1]));
    // Length-prefixed: moving a byte between two keys changes the digest.
    assert_ne!(
        lk1b_is::is_locks_digest([&b"\x04is_i\x01\x02"[..], b"\x04is_m"]),
        lk1b_is::is_locks_digest([&b"\x04is_i\x01"[..], b"\x04is_m\x02"])
    );
    // The empty set is SHA-256 of nothing.
    assert_eq!(lk1b_is::is_locks_digest([]), dash_sha256::sha256(b""),);
}

#[test]
fn core_records_compare_equal_to_themselves() {
    assert_eq!(drive_with(echo), vec![]);
}

#[test]
fn compare_reports_every_plane() {
    // A different reject string.
    let out = drive_with(edit_step(3, |r| {
        rewrite(&r, "conflict-tx-lock", "bad-txns-inputs-missingorspent")
    }));
    assert_eq!(out.len(), 1, "{out:?}");
    assert!(out[0].what.starts_with("outcomes:"), "{out:?}");
    assert_eq!(out[0].step, 3);

    // No records where Core rejected.
    let out = drive_with(edit_step(3, |_| Vec::new()));
    assert!(out.iter().any(|m| m.what.starts_with("outcomes:")), "{out:?}");
    assert!(out.iter().any(|m| m.what.starts_with("rust: no attempt record")), "{out:?}");
    assert!(out.iter().any(|m| m.what.starts_with("is_conflict queries")), "{out:?}");

    // On every attempt only the IS keys of fx count: an extra mn_changed on the reject is not
    // compared, an extra or different is_rm on it is.
    let reject_fx = |fx: &'static str| {
        move |r: Vec<Record>| rewrite(&r, "\"fx\":[]", &format!("\"fx\":[{fx}]"))
    };
    assert_eq!(drive_with(edit_step(3, reject_fx("[\"mn_changed\",false]"))), vec![]);
    let is_rm = concat!(
        "[\"is_rm\",\"618db6faccd19ff4a9c9fc9de5976bf885c220e93ec638f86ea54cc72cd51f50\",",
        "\"0000000000000000000000000000000000000000000000000000000000007702\"]"
    );
    let out = drive_with(edit_step(3, reject_fx(is_rm)));
    assert_eq!(out.len(), 1, "{out:?}");
    assert!(out[0].what.starts_with("fx of "), "{out:?}");
    let mut core = sample();
    core.steps[2].records = reject_fx(is_rm)(core.steps[2].records.clone());
    let out = drive_fixture(&core, edit_step(3, |r| rewrite(&r, "7702\"]]", "7703\"]]")));
    assert_eq!(out.len(), 1, "{out:?}");
    assert!(out[0].what.starts_with("fx of "), "{out:?}");
    // The same on a D: Core's D with no effects, the Rust one with mn_changed.
    let d = |fx: &str| {
        let line = format!(
            "{{\"k\":\"D\",\"h\":104,\"b\":\"{}\",\"org\":\"tip\",\"s\":{{\"res\":\"ok\"}},\"o\":{{\"fx\":[{fx}]}},\"x\":{{\"seq\":9}}}}\n",
            "000000000000000000000000000000000000000000000000000000000000b104"
        );
        parse_line(line.as_bytes()).unwrap()
    };
    let mut core = sample();
    core.steps[3].records.insert(0, d(""));
    let plain = core.clone();
    let out = drive_fixture(&core, |s| {
        let mut o = echo(&plain.steps[s.step - 1]);
        if s.step == 4 {
            o.records[0] = d("[\"mn_changed\",true]");
        }
        o
    });
    assert_eq!(out, vec![]);
    // On the conflict connect too: an extra mn_changed on either side is not compared, a
    // different removal is.
    let add_mn_changed = |r: Vec<Record>| {
        rewrite(&r, "\"fx\":[[\"is_rm\"", "\"fx\":[[\"mn_changed\",false],[\"is_rm\"")
    };
    assert_eq!(drive_with(edit_step(4, add_mn_changed)), vec![]);
    let plain = sample();
    let mut core = plain.clone();
    core.steps[3].records = add_mn_changed(core.steps[3].records.clone());
    assert_eq!(drive_fixture(&core, |s| echo(&plain.steps[s.step - 1])), vec![]);
    let out = drive_with(edit_step(4, |r| rewrite(&r, "7702\"]]", "7703\"]]")));
    assert_eq!(out.len(), 1, "{out:?}");
    assert!(out[0].what.starts_with("fx of "), "{out:?}");

    // fx absent on the Rust side: fine on an ordinary attempt, a mismatch on a conflict attempt.
    let out = drive_with(edit_step(4, |r| {
        let text = encode(&r);
        let start = text.find(",\"o\":{\"fx\":").unwrap();
        let end = start + text[start..].find("]]}").unwrap() + 3;
        parse(&format!("{},\"o\":{{}}{}", &text[..start], &text[end..]))
    }));
    assert_eq!(out.len(), 1, "{out:?}");
    assert_eq!(out[0].what, format!("rust: no fx on conflict attempt {}", hash_of_conflict()));

    // A different is_conflict answer.
    let out = drive_with(edit_step(4, |r| {
        let text = encode(&r);
        let start = text.find("[\"is_conflict\",").unwrap();
        let txid_end = start + "[\"is_conflict\",".len() + 66;
        let pair_end = txid_end + text[txid_end..].find("]]").unwrap() + 2;
        parse(&format!("{},null]{}", &text[..txid_end], &text[pair_end..]))
    }));
    assert_eq!(out.len(), 1, "{out:?}");
    assert!(out[0].what.starts_with("is_conflict queries"), "{out:?}");

    // Checkpoint rows: one missing, one extra, one value changed, and no rows at all.
    let out = drive_with(|s| {
        let mut o = echo(s);
        if let Some(rows) = o.rows.as_mut() {
            let gone = rows.remove(0);
            rows.push((b"\x05is_tx\x09".to_vec(), vec![0x01]));
            rows[0].1 = vec![0x02];
            assert!(gone.0.starts_with(b"\x04is_v"));
        }
        o
    });
    let what: Vec<_> = out.iter().map(|m| m.what.as_str()).collect();
    assert_eq!(what.len(), 1, "{what:?}");
    assert!(what[0].starts_with("checkpoint rows: "), "{what:?}");
    assert_eq!(what[0].matches("; ").count(), 2, "three differences: {what:?}");
    assert!(what[0].contains("core only 0469735f76"), "{what:?}");
    assert!(what[0].contains("rust only 0569735f747809"), "{what:?}");
    let out = drive_with(|s| {
        let mut o = echo(s);
        if let Some(rows) = o.rows.as_mut() {
            rows.retain(|(k, _)| !k.starts_with(b"\x05is_a1"));
        }
        o
    });
    let what: Vec<_> = out.iter().map(|m| m.what.as_str()).collect();
    assert_eq!(what, ["checkpoint: is_locks differs", what[1]], "{what:?}");
    let out = drive_with(|s| Observed {
        rows: None,
        ..echo(s)
    });
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].what, "checkpoint: the Rust side gave no rows");
}

/// The records with every `fx` cut to its IS keys (`is_rm`).
fn is_only_fx(records: &[Record]) -> Vec<Record> {
    let text = encode(records);
    let mut out = String::new();
    let mut rest = text.as_str();
    while let Some(at) = rest.find("\"o\":{\"fx\":") {
        let start = at + "\"o\":{\"fx\":".len();
        let mut values =
            serde_json::Deserializer::from_str(&rest[start..]).into_iter::<serde_json::Value>();
        let mut fx = values.next().unwrap().unwrap();
        let end = start + values.byte_offset();
        fx.as_array_mut().unwrap().retain(|e| e[0] == "is_rm");
        out.push_str(&rest[..start]);
        out.push_str(&fx.to_string());
        rest = &rest[end..];
    }
    out.push_str(rest);
    parse(&out)
}

/// MN0 Q10 on the captures: a Rust side that writes Core's records with IS-only `fx` (every
/// non-IS key Core wrote dropped) compares clean.
#[test]
fn is_only_fx_compares_clean_on_the_captures() {
    use dash_statedigest::Effect;
    let mut keys = std::collections::BTreeSet::new();
    for run in ["main", "rot"] {
        let f = lk1b_is::capture(run);
        let mut changed = 0;
        for s in &f.steps {
            let trimmed = is_only_fx(&s.records);
            let core = lk1b_is::attempts(&s.records);
            let rust = lk1b_is::attempts(&trimmed);
            assert_eq!(core.len(), rust.len());
            for (c, r) in core.iter().zip(&rust) {
                let (Some(cf), Some(rf)) = (&c.fx, &r.fx) else {
                    assert_eq!(c.fx.is_some(), r.fx.is_some());
                    continue;
                };
                assert_eq!(rf, &lk1b_is::is_effects(cf), "step {}", s.step);
                if cf != rf {
                    changed += 1;
                    keys.extend(cf.iter().filter(|e| !rf.contains(e)).map(|e| match e {
                        Effect::Snap {
                            ..
                        } => "snap",
                        Effect::IsRm {
                            ..
                        } => "is_rm",
                        Effect::QcUnmine {
                            ..
                        } => "qc_unmine",
                        Effect::QcRemine {
                            ..
                        } => "qc_remine",
                        Effect::BanReset {
                            ..
                        } => "ban_reset",
                        Effect::MnChanged {
                            ..
                        } => "mn_changed",
                    }));
                }
            }
        }
        assert!(changed > 0, "{run}");
        let out = drive_fixture(&f, |s| Observed {
            records: is_only_fx(&s.records),
            ..echo(s)
        });
        assert_eq!(out, vec![], "{run}");
    }
    assert_eq!(
        keys.into_iter().collect::<Vec<_>>(),
        ["ban_reset", "mn_changed", "qc_unmine", "snap"],
        "the non-IS keys the captures drop"
    );
}

/// MN0 Q10, one non-IS key at a time: on either side of the reject or the conflict connect it
/// is not compared.
#[test]
fn every_non_is_key_is_left_out() {
    let non_is = [
        "[\"mn_changed\",false]",
        concat!(
            "[\"snap\",103,\"1d1e0bc17ca822edf202af62b150e07193887555e82775a4c5208c1130d51023\",",
            "\"af72be27c06a1cdf77879c61665f8197d3a7c5048d7c0c6f68e3c76f699f2d36\"]"
        ),
        "[\"qc_unmine\",100,\"30b16a311758dac535378093c236f06370c775332e21f7fc3afbeeeba7b748fa\"]",
        "[\"ban_reset\",\"94aec84408e20f1970417e9003c1f3f3dd7b19f0644f9acc480eb27a2d4491a4\",114,false]",
    ];
    for key in non_is {
        let on_reject = move |r: Vec<Record>| rewrite(&r, "\"fx\":[]", &format!("\"fx\":[{key}]"));
        let on_connect = move |r: Vec<Record>| {
            rewrite(&r, "\"fx\":[[\"is_rm\"", &format!("\"fx\":[{key},[\"is_rm\""))
        };
        assert_eq!(drive_with(edit_step(3, on_reject)), vec![], "rust R {key}");
        assert_eq!(drive_with(edit_step(4, on_connect)), vec![], "rust C {key}");
        let plain = sample();
        let mut core = plain.clone();
        core.steps[2].records = on_reject(core.steps[2].records.clone());
        core.steps[3].records = on_connect(core.steps[3].records.clone());
        assert_eq!(drive_fixture(&core, |s| echo(&plain.steps[s.step - 1])), vec![], "core {key}");
    }
}

/// A `D` line of block `b104` with `res` and `fx` as given.
fn d_record(org: &str, res: &str, fx: &str) -> Record {
    let line = format!(
        "{{\"k\":\"D\",\"h\":104,\"b\":\"{}\",\"org\":\"{org}\",\"s\":{{\"res\":\"{res}\"}},\"o\":{{\"fx\":[{fx}]}},\"x\":{{\"seq\":9}}}}\n",
        "000000000000000000000000000000000000000000000000000000000000b104"
    );
    parse_line(line.as_bytes()).unwrap()
}

#[test]
fn every_outcome_field_and_the_kind_count() {
    // An R with another cls.
    let out =
        drive_with(edit_step(3, |r| rewrite(&r, "\"cls\":\"chainlock\"", "\"cls\":\"consensus\"")));
    assert_eq!(out.len(), 1, "{out:?}");
    assert!(out[0].what.starts_with("outcomes:"), "{out:?}");
    // A D with another res.
    let with_d = |res: &'static str| {
        move |mut r: Vec<Record>| {
            r.insert(0, d_record("tip", res, ""));
            r
        }
    };
    let mut core = sample();
    core.steps[3].records = with_d("ok")(core.steps[3].records.clone());
    let plain = core.clone();
    assert_eq!(drive_fixture(&core, |s| echo(&plain.steps[s.step - 1])), vec![]);
    let out = drive_fixture(&core, |s| {
        let mut o = echo(&plain.steps[s.step - 1]);
        if s.step == 4 {
            o.records[0] = d_record("tip", "unclean", "");
        }
        o
    });
    assert_eq!(out.len(), 1, "{out:?}");
    assert!(out[0].what.starts_with("outcomes:"), "{out:?}");
    // A C reported where Core wrote the R: step 4's C of the same block, with step 3's A.
    let plain = sample();
    let out = drive_with(|s| {
        let mut o = echo(s);
        if s.step == 3 {
            o.records[0] = plain.steps[3].records[0].clone();
        }
        o
    });
    assert!(out.iter().any(|m| m.what.starts_with("outcomes:")), "{out:?}");
}

#[test]
fn verifydb_queries_are_not_compared() {
    // VerifyDB's A records with different queries on the two sides.
    let verifydb_a = |ibd: bool| {
        let line = format!(
            "{{\"k\":\"A\",\"h\":104,\"b\":\"{}\",\"org\":\"verifydb\",\"t\":1700000005,\"q\":[[\"ibd\",{ibd}],[\"is_conflict\",\"{}\",null]],\"x\":{{\"seq\":9}}}}\n",
            "000000000000000000000000000000000000000000000000000000000000b104",
            "0000000000000000000000000000000000000000000000000000000000007709"
        );
        parse_line(line.as_bytes()).unwrap()
    };
    let mut core = sample();
    core.steps[4].records.push(verifydb_a(false));
    let mut rust = sample();
    rust.steps[4].records.push(verifydb_a(true));
    rust.steps[4].records.push(verifydb_a(true));
    assert_eq!(drive_fixture(&core, |s| echo(&rust.steps[s.step - 1])), vec![]);
}

#[test]
fn conflict_attempts_take_the_last_non_disconnect() {
    let strip = |r: &Record| {
        parse(&encode(std::slice::from_ref(r)).replace(",\"o\":{\"fx\":[]}", ",\"o\":{}")).remove(0)
    };
    // A conflict R without fx, then the C with it: the C counts.
    let mut f = sample();
    let reject = strip(&f.steps[2].records[0]);
    f.steps[3].records.insert(0, reject);
    let plain = f.clone();
    assert_eq!(drive_fixture(&f, |s| echo(&plain.steps[s.step - 1])), vec![]);
    // A conflict block whose only Rust record is a D (with fx) has no attempt.
    let hash = sample().steps[2].conflict[0].to_hex();
    let d = parse_line(
        format!(
            "{{\"k\":\"D\",\"h\":105,\"b\":\"{hash}\",\"org\":\"tip\",\"s\":{{\"res\":\"ok\"}},\"o\":{{\"fx\":[]}},\"x\":{{\"seq\":2}}}}\n"
        )
        .as_bytes(),
    )
    .unwrap();
    let out = drive_with(edit_step(3, |r| {
        let mut r = r;
        r[0] = d.clone();
        r
    }));
    assert!(
        out.iter().any(|m| m.what == format!("rust: no attempt record for conflict block {hash}")),
        "{out:?}"
    );
    // The loader refuses a fixture whose conflict block has only a D.
    let d_line = encode(std::slice::from_ref(&d));
    refused(
        |f| f["steps"][2]["digest"] = serde_json::json!([d_line.trim_end()]),
        "step 3: conflict attempt",
    );
}

#[test]
fn checkpoint_rows_must_not_repeat_a_key() {
    refused(
        |f| {
            let rows = f["steps"][4]["checkpoint"]["rows"].as_array_mut().unwrap();
            let first = rows[0].clone();
            rows.insert(1, first);
        },
        "rows not in strict key order",
    );
}

#[test]
fn periodic_records_are_retries_of_ds_chainlocked_block() {
    // The sample's (c) steps relabelled (d): step 4 is the ChainLock of the conflicting block and
    // carries a reject of it; its retries ride on the next step of (d).
    let mut f: serde_json::Value = serde_json::from_str(&sample_text()).unwrap();
    for i in 1..5 {
        f["steps"][i]["scenario"] = serde_json::json!("d");
    }
    let retry = f["steps"][2]["digest"][0].clone();
    let retry_a = f["steps"][2]["decisions"][0].clone();
    f["steps"][3]["digest"].as_array_mut().unwrap().push(retry.clone());
    f["steps"][3]["decisions"].as_array_mut().unwrap().push(retry_a.clone());
    f["steps"][4]["periodic"] = serde_json::json!([retry, retry_a]);
    lk1b_is::load(&f.to_string()).expect("retries in (d) load");
    let refused = |f: &serde_json::Value| {
        let e = lk1b_is::load(&f.to_string()).unwrap_err();
        assert!(e.contains("not a retry"), "{e}");
    };
    // A retry with another reason, one that removes a lock, and an A of another block are not.
    let mut g = f.clone();
    g["steps"][4]["periodic"] =
        serde_json::json!([retry.as_str().unwrap().replace("conflict-tx-lock", "bad-cb-payee")]);
    refused(&g);
    let mut g = f.clone();
    g["steps"][4]["periodic"] = f["steps"][3]["digest"].clone();
    refused(&g);
    let mut g = f.clone();
    g["steps"][4]["periodic"] =
        serde_json::json!([retry_a.as_str().unwrap().replace("f62e\",\"org", "f62f\",\"org")]);
    refused(&g);
    // A retry with the right outcome that removes a lock.
    let mut g = f.clone();
    let is_rm = concat!(
        "\"fx\":[[\"is_rm\",\"618db6faccd19ff4a9c9fc9de5976bf885c220e93ec638f86ea54cc72cd51f50\",",
        "\"0000000000000000000000000000000000000000000000000000000000007702\"]]"
    );
    g["steps"][4]["periodic"] =
        serde_json::json!([retry.as_str().unwrap().replace("\"fx\":[]", is_rm)]);
    refused(&g);
    // Retries of a ChainLocked block whose first attempt connected it (the A alone, which names the
    // block).
    let mut g: serde_json::Value = serde_json::from_str(&sample_text()).unwrap();
    for i in 1..5 {
        g["steps"][i]["scenario"] = serde_json::json!("d");
    }
    g["steps"][4]["periodic"] = serde_json::json!([retry_a]);
    refused(&g);
    // Periodic records on an (f) step before any restart.
    let mut g = f.clone();
    let mut f_step = f["steps"][0].clone();
    f_step["scenario"] = serde_json::json!("f");
    f_step["periodic"] = serde_json::json!([retry]);
    g["steps"][4].as_object_mut().unwrap().remove("periodic");
    g["steps"].as_array_mut().unwrap().insert(4, f_step);
    for (i, s) in g["steps"].as_array_mut().unwrap().iter_mut().enumerate() {
        s["step"] = serde_json::json!(i + 1);
    }
    refused(&g);
    // After the restart there are no retries, though the scenario is still (d).
    let mut g = f.clone();
    let mut after = f["steps"][0].clone();
    after["step"] = serde_json::json!(6);
    after["scenario"] = serde_json::json!("d");
    after["digest"] = serde_json::json!([]);
    after["periodic"] = serde_json::json!([retry]);
    g["steps"].as_array_mut().unwrap().push(after);
    refused(&g);
}

#[test]
fn verifydb_records_are_not_compared() {
    let verifydb = concat!(
        r#"{"k":"D","h":104,"b":"000000000000000000000000000000000000000000000000000000000000b104","#,
        r#""org":"verifydb","s":{"res":"ok"},"o":{"fx":[]},"x":{"seq":9}}"#,
        "\n"
    );
    let out = drive_with(edit_step(5, |mut r| {
        r.push(parse_line(verifydb.as_bytes()).unwrap());
        r
    }));
    assert_eq!(out, vec![]);
}

#[test]
fn fx_is_optional_on_an_ordinary_attempt() {
    // Step 3's R is not followed by a conflict entry in this edited copy, so an `fx`-less Rust R
    // passes; with the entry (the sample as is) it is a mismatch.
    let strip = |r: Vec<Record>| rewrite(&r, ",\"o\":{\"fx\":[]}", ",\"o\":{}");
    let out = drive_with(edit_step(3, strip));
    assert_eq!(out.len(), 1, "{out:?}");
    assert_eq!(out[0].what, format!("rust: no fx on conflict attempt {}", hash_of_conflict()));
    let mut fixture = sample();
    fixture.steps[2].conflict.clear();
    assert_eq!(drive_fixture(&fixture, edit_step(3, strip)), vec![]);
}

fn hash_of_conflict() -> String {
    sample().steps[2].conflict[0].to_hex()
}

/// MN0 Q11 at the KILL step: a Rust node that kept (g)'s killed block connects it as it starts,
/// so the KILL step may carry Core's records of the re-delivery after its own, as startup's
/// import writes them (no `is_conflict` query, the connect being in IBD); the re-delivery is then
/// a duplicate.
#[test]
fn the_kill_step_may_connect_the_kept_block_as_startups_import_does() {
    let main = lk1b_is::capture("main");
    let marked: Vec<_> =
        main.steps.iter().filter(|s| s.kill_replay.is_some()).map(|s| s.step).collect();
    assert_eq!(marked, [256]);
    assert!(matches!(
        main.steps[255].op,
        Op::Restart {
            kind: RestartKind::Kill,
            ..
        }
    ));
    let delivered = main.steps[256].records.clone();
    assert!(!lk1b_is::attempts(&delivered).is_empty(), "the re-delivery connects");
    assert!(
        lk1b_is::is_conflict_queries(&delivered).iter().any(|(_, q)| !q.is_empty()),
        "the re-delivery asks is_conflict"
    );
    let replay = main.steps[255].kill_replay.clone().expect("the KILL's replay");
    assert!(lk1b_is::is_conflict_queries(&replay).iter().all(|(_, q)| q.is_empty()));
    let kept = |extra: Vec<Record>| {
        move |s: &Step| {
            let mut o = echo(s);
            match s.step {
                256 => o.records.extend(extra.clone()),
                257 => o.records.clear(),
                _ => {}
            }
            o
        }
    };
    assert_eq!(drive_fixture(&main, kept(replay.clone())), vec![]);
    // The connect as a re-delivery out of IBD writes it, its `is_conflict` query included, is a
    // mismatch at the KILL: startup's import cannot ask.
    let out = drive_fixture(&main, kept(delivered));
    assert!(!out.is_empty() && out.iter().all(|m| m.step == 256), "{out:?}");
    // The connect without its A is a mismatch at the KILL.
    let mut no_a = replay.clone();
    no_a.retain(|r| !matches!(r, Record::Attempt(_)));
    let out = drive_fixture(&main, kept(no_a));
    assert!(!out.is_empty() && out.iter().all(|m| m.step == 256), "{out:?}");
    // Only the KILL step takes them: the same records one restart earlier are a mismatch.
    let early = drive_fixture(&main, move |s: &Step| {
        let mut o = echo(s);
        if s.step == 253 {
            o.records.extend(replay.clone());
        }
        o
    });
    assert!(early.iter().all(|m| m.step == 253) && !early.is_empty(), "{early:?}");
}

/// MN0 Q11: on main's kill-dependent step (the block (g)'s KILL lost, delivered again) the Rust
/// side may connect it as Core did or skip it as a duplicate; its IS rows must equal the KILL
/// checkpoint's and its tip the block.
#[test]
fn kill_dependent_step_compares_the_state() {
    let main = lk1b_is::capture("main");
    let marked: Vec<_> =
        main.steps.iter().filter(|s| s.kill_dependent.is_some()).map(|s| s.step).collect();
    assert_eq!(marked, [257]);
    let at_257 = |f: fn(&mut Observed)| {
        move |s: &Step| {
            let mut o = echo(s);
            if s.step == 257 {
                f(&mut o);
            }
            o
        }
    };
    let steps = |out: &[Mismatch]| out.iter().map(|m| m.step).collect::<Vec<_>>();
    // Core's C with its A, and no attempt at all.
    assert_eq!(drive_fixture(&main, echo), vec![]);
    assert_eq!(drive_fixture(&main, at_257(|o| o.records.clear())), vec![]);
    // The C without its A is neither.
    let out =
        drive_fixture(&main, at_257(|o| o.records.retain(|r| !matches!(r, Record::Attempt(_)))));
    assert_eq!(steps(&out), [257], "{out:?}");
    assert!(out[0].what.starts_with("is_conflict queries"), "{out:?}");
    // Wrong rows: the is_m row the re-delivery rewrites is missing, after a connect or a skip.
    let no_is_m = |o: &mut Observed| {
        o.rows.as_mut().unwrap().retain(|(k, _)| lk1b_is::table(k) != Some("is_m"));
    };
    for out in [
        drive_fixture(&main, at_257(no_is_m)),
        drive_fixture(
            &main,
            at_257(|o| {
                o.records.clear();
                o.rows.as_mut().unwrap().retain(|(k, _)| lk1b_is::table(k) != Some("is_m"));
            }),
        ),
    ] {
        assert_eq!(steps(&out), [257, 257], "{out:?}");
        assert_eq!(out[0].what, "kill-dependent state: is_locks differs");
        assert!(
            out[1].what.starts_with("kill-dependent state rows: core only 0469735f6d"),
            "{out:?}"
        );
    }
    let out = drive_fixture(&main, at_257(|o| o.rows = None));
    assert_eq!(steps(&out), [257], "{out:?}");
    assert_eq!(out[0].what, "kill-dependent state: the Rust side gave no rows");
    // Wrong tip, and none.
    let out = drive_fixture(&main, at_257(|o| o.tip = Some(Hash256::from_wire_bytes([1; 32]))));
    assert_eq!(steps(&out), [257], "{out:?}");
    assert!(out[0].what.starts_with("kill-dependent state: tip 0101"), "{out:?}");
    let out = drive_fixture(&main, at_257(|o| o.tip = None));
    assert_eq!(steps(&out), [257], "{out:?}");
    assert_eq!(out[0].what, "kill-dependent state: the Rust side gave no tip");
    // The A without its C is not a duplicate either.
    let out =
        drive_fixture(&main, at_257(|o| o.records.retain(|r| matches!(r, Record::Attempt(_)))));
    assert_eq!(steps(&out), [257], "{out:?}");
    assert!(out[0].what.starts_with("outcomes:"), "{out:?}");
}

/// The sample with its RESTART turned into a KILL, then `after` (each a BLOCK step, by op), and
/// for `run` `main` or `rot` a SPORK step per scenario the run must hold; renumbered.
fn killed_sample(run: &str, after: &[serde_json::Value]) -> serde_json::Value {
    let mut f: serde_json::Value = serde_json::from_str(&sample_text()).unwrap();
    f["run"] = serde_json::json!(run);
    f["steps"][4]["op"]["kind"] = serde_json::json!("KILL");
    let template = f["steps"][2].clone();
    let spork = f["steps"][0].clone();
    let steps = f["steps"].as_array_mut().unwrap();
    for op in after {
        let mut s = template.clone();
        s["op"] = op.clone();
        s["digest"] = serde_json::json!([]);
        s["decisions"] = serde_json::json!([]);
        s["conflict"] = serde_json::Value::Null;
        steps.push(s);
    }
    for scenario in lk1b_is::expected_scenarios(run) {
        let mut s = spork.clone();
        s["scenario"] = serde_json::json!(scenario);
        s["digest"] = serde_json::json!([]);
        steps.push(s);
    }
    for (i, s) in steps.iter_mut().enumerate() {
        s["step"] = serde_json::json!(i + 1);
    }
    f
}

/// MN0 Q11's mark: the first BLOCK after a KILL, only if a C connected its block before the KILL,
/// and exactly one such step in main and none elsewhere.
#[test]
fn kill_dependent_mark_is_the_first_block_after_the_kill() {
    let f: serde_json::Value = serde_json::from_str(&sample_text()).unwrap();
    // The sample's block, which step 4's C connects, and a new block (another nonce).
    let old = f["steps"][2]["op"].clone();
    let mut new = old.clone();
    let mut bytes = unhex(new["bytes"].as_str().unwrap());
    bytes[76] ^= 1;
    let header = dash_consensus::decode_header(&mut &bytes[..80]).unwrap();
    let hash = Hash256::from_wire_bytes(*header.value().hash().as_bytes());
    new["hash"] = serde_json::json!(hash.to_hex());
    new["bytes"] = serde_json::json!(bytes.iter().map(|b| format!("{b:02x}")).collect::<String>());
    let marked = |f: &serde_json::Value| {
        let f = lk1b_is::load(&f.to_string()).unwrap_or_else(|e| panic!("{e}"));
        for s in f.steps.iter().filter(|s| s.kill_dependent.is_some()) {
            assert_eq!(s.kill_dependent, f.steps[4].checkpoint, "step {}", s.step);
        }
        f.steps.iter().filter(|s| s.kill_dependent.is_some()).map(|s| s.step).collect::<Vec<_>>()
    };
    let refused = |f: &serde_json::Value, want: &str| {
        let e = lk1b_is::load(&f.to_string()).expect_err(want);
        assert!(e.contains(want), "want {want:?}: {e}");
    };
    // The re-delivery is marked, a second one after it is not.
    assert_eq!(marked(&killed_sample("main", &[old.clone(), old.clone()])), [6]);
    // A new block first: nothing is marked, not even the re-delivery after it.
    assert_eq!(marked(&killed_sample("sample", &[new.clone(), old.clone()])), Vec::<usize>::new());
    refused(
        &killed_sample("main", &[new, old.clone()]),
        "fixture main: 0 kill-dependent steps, want 1",
    );
    // Two KILLs each followed by the re-delivery, and a rot with one.
    let mut two = killed_sample("main", std::slice::from_ref(&old));
    let kill = two["steps"][4].clone();
    let redelivery = two["steps"][5].clone();
    let steps = two["steps"].as_array_mut().unwrap();
    steps.insert(6, redelivery);
    steps.insert(6, kill);
    for (i, s) in steps.iter_mut().enumerate() {
        s["step"] = serde_json::json!(i + 1);
    }
    refused(&two, "fixture main: 2 kill-dependent steps, want 1");
    refused(&killed_sample("rot", &[old]), "fixture rot: 1 kill-dependent steps, want 0");
}

fn unhex(text: &str) -> Vec<u8> {
    dash_statedigest::decode_hex(text.as_bytes()).expect("hex")
}

#[test]
fn row_diff_reports_by_key() {
    let core: Rows = vec![(vec![1], vec![1]), (vec![2], vec![2])];
    let rust: Rows = vec![(vec![2], vec![3]), (vec![4], vec![4])];
    assert_eq!(
        lk1b_is::row_diff(&core, &rust),
        ["core only 01", "02: core 02, rust 03", "rust only 04"]
    );
    assert!(lk1b_is::row_diff(&core, &core).is_empty());
}

/// NodeSut behind [`IsSide`] ([`lk1b_is::node::NodeSide`]).
#[cfg(feature = "test-support")]
fn node_sut_side(fixture: &Fixture) -> Result<Box<dyn IsSide>, String> {
    Ok(Box::new(lk1b_is::node::NodeSide::new(fixture, Default::default())?))
}

fn sha256d(bytes: &[u8]) -> [u8; 32] {
    let mut h = dash_sha256::Sha256d::new();
    h.update(bytes);
    h.finalize()
}

/// A string field of an attempt's outcome (`res`, `cls`, `why`).
fn outcome_str(a: &lk1b_is::AttemptView, key: &str) -> String {
    match a.outcome.iter().find(|(k, _)| *k == key).map(|(_, v)| v) {
        Some(dash_statedigest::Value::Str(b)) => String::from_utf8(b.clone()).unwrap(),
        other => format!("{other:?}"),
    }
}

/// The outpoints (36 bytes each: txid, index) an `isdlock` payload locks.
fn islock_inputs(bytes: &[u8]) -> Vec<&[u8]> {
    let count = usize::from(bytes[1]);
    assert!(count < 0xfd);
    (0..count).map(|i| &bytes[2 + 36 * i..2 + 36 * (i + 1)]).collect()
}

/// Each transaction of a block's bytes: its txid (wire order) and its serialization.
fn block_txs(bytes: &[u8]) -> Vec<([u8; 32], Vec<u8>)> {
    let count = usize::from(bytes[80]);
    assert!(count < 0xfd);
    let mut cursor = &bytes[81..];
    (0..count)
        .map(|_| {
            let wire = dash_consensus::decode_tx(&mut cursor).expect("block tx").wire().to_vec();
            (sha256d(&wire), wire)
        })
        .collect()
}

/// The facts each main scenario was captured for, by step label and lock hash (`sha256d` of the
/// `isdlock` payload). The IS rows of a step are those of the first checkpoint at or after it.
fn pin_main_scenarios(main: &Fixture) {
    use dash_statedigest::{Effect, Kind, Query};
    let step = |scenario: &str, label: &str| -> &Step {
        let found: Vec<_> =
            main.steps.iter().filter(|s| s.scenario == scenario && s.label == label).collect();
        assert_eq!(found.len(), 1, "({scenario}) {label}");
        found[0]
    };
    let lock = |scenario: &str, label: &str| -> [u8; 32] {
        let Op::IsLock {
            bytes,
            ..
        } = &step(scenario, label).op
        else {
            panic!("({scenario}) {label} is not an ISLOCK")
        };
        sha256d(bytes)
    };
    let block_hash = |s: &Step| match &s.op {
        Op::Block {
            hash,
            ..
        } => *hash,
        op => panic!("step {} is a {}", s.step, op.tag()),
    };
    let rows_after = |s: &Step| -> &Rows {
        &main.steps[s.step - 1..]
            .iter()
            .find_map(|s| s.checkpoint.as_ref())
            .expect("checkpoint")
            .rows
    };
    // A row of `table` naming the lock: in its key (is_i, is_m, is_a1, is_a2) or as its value
    // (is_tx, is_in).
    let names = |rows: &Rows, table: &str, lock: &[u8; 32]| {
        rows.iter().any(|(k, v)| {
            lk1b_is::table(k) == Some(table) && (k.ends_with(lock) || v.as_slice() == lock)
        })
    };
    let kinds = |s: &Step| lk1b_is::attempts(&s.records).iter().map(|a| a.kind).collect::<Vec<_>>();
    // The height of the last C before the ChainLock step whose block mines the lock's tx.
    let mining_height = |scenario: &str, lock_label: &str, cl: &Step| -> u32 {
        let Op::IsLock {
            txid,
            ..
        } = &step(scenario, lock_label).op
        else {
            panic!("({scenario}) {lock_label} is not an ISLOCK")
        };
        let mines = |b: &Hash256| {
            main.steps.iter().any(|s| match &s.op {
                Op::Block {
                    hash,
                    bytes,
                    ..
                } => {
                    hash == b
                        && block_txs(bytes)
                            .iter()
                            .any(|(t, _)| Hash256::from_wire_bytes(*t) == *txid)
                }
                _ => false,
            })
        };
        let c = main.steps[..cl.step - 1]
            .iter()
            .flat_map(|s| lk1b_is::attempts(&s.records))
            .rfind(|a| a.kind == Kind::Connect && mines(&a.id.b))
            .unwrap_or_else(|| panic!("({scenario}) no C mining the lock's tx"));
        u32::try_from(c.id.h).unwrap()
    };
    // The height in the lock's only is_a1 key (the inverted big-endian height, then the hash).
    let a1_height = |rows: &Rows, lock: &[u8; 32]| -> u32 {
        let keys: Vec<_> = rows
            .iter()
            .map(|(k, _)| k)
            .filter(|k| lk1b_is::table(k) == Some("is_a1") && k.ends_with(lock))
            .collect();
        assert_eq!(keys.len(), 1, "is_a1 keys of the lock");
        let k = keys[0];
        !u32::from_be_bytes(k[k.len() - 36..k.len() - 32].try_into().unwrap())
    };

    // (a): the ChainLock archives the lock (is_a1, is_a2) and drops it from is_i.
    let a_lock = lock("a", "lock");
    let a_cl = step("a", "ChainLock of the mining block");
    let a_rows = rows_after(a_cl);
    assert!(names(a_rows, "is_a1", &a_lock) && names(a_rows, "is_a2", &a_lock), "(a) archive");
    assert!(!names(a_rows, "is_i", &a_lock), "(a) is_i");
    assert_eq!(a1_height(a_rows, &a_lock), mining_height("a", "lock", a_cl), "(a) is_a1 height");

    // (b): the conflicting block's R, the producer block at its height next, and the rejected
    // block never connected later.
    let b_block = step("b", "conflicting block");
    let b_r = lk1b_is::attempts(&b_block.records);
    assert_eq!(kinds(b_block), [Kind::Reject], "(b)");
    assert_eq!(outcome_str(&b_r[0], "why"), "conflict-tx-lock", "(b) why");
    assert_eq!(outcome_str(&b_r[0], "cls"), "chainlock", "(b) cls");
    let next = lk1b_is::attempts(&main.steps[b_block.step].records);
    assert!(
        matches!(&next[..], [c] if c.kind == Kind::Connect && c.id.h == b_r[0].id.h && c.id.b != b_r[0].id.b),
        "(b) the next step: {next:?}"
    );
    assert!(
        main.steps[b_block.step..].iter().all(|s| lk1b_is::attempts(&s.records)
            .iter()
            .all(|a| a.kind != Kind::Connect || a.id.b != b_r[0].id.b)),
        "(b) the rejected block connected later"
    );

    // (c): the ChainLock's enforcement connects the conflicting block and removes the lock and its
    // chained child.
    let c_cl = step("c", "ChainLock of the conflicting block");
    assert_eq!(kinds(c_cl), [Kind::Connect], "(c)");
    let c_rows = rows_after(c_cl);
    for label in ["lock", "chained child lock"] {
        assert!(!names(c_rows, "is_i", &lock("c", label)), "(c) {label} kept");
    }

    // (c'): CL(X) removes A for T, the tx of X that spends y (A's outpoint B does not lock); B
    // stays with its is_tx row and without an is_in row.
    let x_cl = step("c'", "ChainLock of X");
    let a = lock("c'", "lock A on x and y");
    let b = lock("c'", "lock B on x (mined)");
    let x_rows = rows_after(x_cl);
    assert!(names(x_rows, "is_i", &b) && names(x_rows, "is_tx", &b), "(c') B kept");
    assert!(!names(x_rows, "is_in", &b), "(c') B's is_in row");
    let a_bytes = match &step("c'", "lock A on x and y").op {
        Op::IsLock {
            bytes,
            ..
        } => bytes.clone(),
        _ => unreachable!(),
    };
    let b_bytes = match &step("c'", "lock B on x (mined)").op {
        Op::IsLock {
            bytes,
            ..
        } => bytes.clone(),
        _ => unreachable!(),
    };
    let b_inputs = islock_inputs(&b_bytes);
    let y: Vec<_> = islock_inputs(&a_bytes).into_iter().filter(|o| !b_inputs.contains(o)).collect();
    assert_eq!(y.len(), 1, "(c') y");
    let x_block = step("c'", "X: T spends y, U spends x (stored, not connected)");
    let Op::Block {
        bytes: x_bytes,
        ..
    } = &x_block.op
    else {
        unreachable!()
    };
    let spends = |wire: &[u8], outpoint: &[u8]| wire.windows(36).any(|w| w == outpoint);
    let t: Vec<_> = block_txs(x_bytes).into_iter().filter(|(_, w)| spends(w, y[0])).collect();
    assert_eq!(t.len(), 1, "(c') T");
    assert!(!spends(&t[0].1, b_inputs[0]), "(c') T spends x");
    let x_c = lk1b_is::attempts(&x_cl.records)
        .into_iter()
        .find(|a| a.kind == Kind::Connect)
        .expect("(c') C(X)");
    assert_eq!(
        lk1b_is::is_effects(x_c.fx.as_deref().unwrap()),
        [Effect::IsRm {
            islock: Hash256::from_wire_bytes(a),
            txid: Hash256::from_wire_bytes(t[0].0),
        }],
        "(c') is_rm"
    );

    // (d): the ChainLocked reject removes the lock.
    let d_cl = step("d", "ChainLock of the bad block");
    assert!(!names(rows_after(d_cl), "is_i", &lock("d", "lock")), "(d) lock kept");

    // (e): in IBD the conflicting block connects and the lock stays.
    let e_block = step("e", "conflicting block in IBD");
    let ibd: Vec<_> = e_block
        .records
        .iter()
        .filter_map(|r| match r {
            Record::Attempt(a) => Some(a.q.iter().filter(|q| matches!(q, Query::Ibd(_)))),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(ibd, [&Query::Ibd(true)], "(e) ibd");
    assert_eq!(kinds(e_block), [Kind::Connect], "(e)");
    assert!(names(rows_after(e_block), "is_i", &lock("e", "lock")), "(e) lock removed");

    // (f): the producer blocks disconnect the recorder-only block.
    let recorder_only = block_hash(step("f", "recorder-only block mining the locked tx"));
    let reorg = step("f", "producer block 219");
    assert!(
        lk1b_is::attempts(&reorg.records)
            .iter()
            .any(|a| a.kind == Kind::Disconnect && a.id.b == recorder_only),
        "(f) no D of the recorder-only block"
    );
    // No checkpoint lies between the reorg and the ChainLock, so the D pin and the is_a1 height
    // pin below cover (f).
    // The archive names the re-mining block's height, not the recorder-only block's.
    let f_cl = step("f", "ChainLock of the re-mining block");
    assert_eq!(
        a1_height(rows_after(f_cl), &lock("f", "lock")),
        mining_height("f", "lock", f_cl),
        "(f) is_a1 height"
    );

    // (g): the BLOCK after the KILL is the last block connected before it, and the KILL's
    // checkpoint already holds that block's is_m row for (g)'s lock.
    let kill = main
        .steps
        .iter()
        .position(|s| {
            matches!(
                s.op,
                Op::Restart {
                    kind: RestartKind::Kill,
                    ..
                }
            )
        })
        .expect("(g) KILL");
    let last_c = main.steps[..kill]
        .iter()
        .rev()
        .find_map(|s| lk1b_is::attempts(&s.records).into_iter().rfind(|a| a.kind == Kind::Connect))
        .expect("(g) a C before the KILL");
    assert_eq!(block_hash(&main.steps[kill + 1]), last_c.id.b, "(g) re-delivery");
    let height = !u32::try_from(last_c.id.h).unwrap();
    let is_m = [&b"\x04is_m"[..], &height.to_be_bytes(), &lock("g", "lock")].concat();
    let kill_rows = &main.steps[kill].checkpoint.as_ref().expect("(g) checkpoint").rows;
    assert!(kill_rows.iter().any(|(k, _)| *k == is_m), "(g) is_m in the KILL checkpoint");

    // (h): the rows (a) archived are there before pruning and gone after.
    let archived: Vec<_> = a_rows
        .iter()
        .filter(|(k, _)| {
            matches!(lk1b_is::table(k), Some("is_a1" | "is_a2")) && k.ends_with(&a_lock)
        })
        .collect();
    assert_eq!(archived.len(), 2, "(a) archive rows");
    let before = &step("h", "checkpoint: before pruning").checkpoint.as_ref().unwrap().rows;
    let after = &step("h", "checkpoint: (a)'s archive pruned").checkpoint.as_ref().unwrap().rows;
    assert!(archived.iter().all(|r| before.contains(r)), "(h) before pruning");
    assert!(archived.iter().all(|r| !after.contains(r)), "(h) pruned");
}

/// The captures load (every check of `lk1b_is::load`, the periodic rule included), hold the
/// scenarios they were captured for, and compare clean against themselves.
#[test]
fn capture_fixtures_load_and_self_compare() {
    let main = lk1b_is::capture("main");
    assert_eq!(main.run, "main");
    pin_main_scenarios(&main);
    for scenario in ["setup", "a", "b", "c", "c'", "d", "f", "g", "e", "h"] {
        assert!(main.steps.iter().any(|s| s.scenario == scenario), "main lacks ({scenario})");
    }
    assert!(main.steps.iter().any(|s| matches!(
        s.op,
        Op::Restart {
            kind: RestartKind::Kill,
            ..
        }
    )));
    // (b), (c), (c'), (d): one conflict attempt each with Core's fx; the connects remove a lock.
    let conflicts: Vec<_> = main.steps.iter().filter(|s| !s.conflict.is_empty()).collect();
    for scenario in ["b", "c", "c'", "d"] {
        assert!(
            conflicts.iter().any(|s| s.scenario == scenario),
            "no conflict step in ({scenario})"
        );
    }
    for step in &conflicts {
        for b in &step.conflict {
            let a = lk1b_is::attempts(&step.records).into_iter().rfind(|a| &a.id.b == b).unwrap();
            let is_rm =
                a.fx.as_ref()
                    .unwrap()
                    .iter()
                    .filter(|e| matches!(e, dash_statedigest::Effect::IsRm { .. }))
                    .count();
            match (step.scenario.as_str(), &step.op) {
                (
                    _,
                    Op::Block {
                        ..
                    },
                ) => assert_eq!(is_rm, 0, "step {}", step.step),
                (
                    "c" | "c'" | "d",
                    Op::ClRecord {
                        ..
                    },
                ) => assert_eq!(is_rm, 1, "step {}", step.step),
                _ => panic!("unexpected conflict step {}", step.step),
            }
        }
    }
    let by_label = |label: &str| {
        main.steps.iter().find(|s| s.label == label).unwrap_or_else(|| panic!("{label}"))
    };
    let reject_why = |s: &Step| {
        lk1b_is::attempts(&s.records)
            .iter()
            .filter(|a| a.kind == dash_statedigest::Kind::Reject)
            .flat_map(|a| {
                a.outcome
                    .iter()
                    .filter(|(k, _)| *k == "why")
                    .map(|(_, v)| v.clone())
                    .collect::<Vec<_>>()
            })
            .map(|v| match v {
                dash_statedigest::Value::Str(b) => String::from_utf8(b).unwrap(),
                other => format!("{other:?}"),
            })
            .collect::<Vec<_>>()
    };
    // (d): the ChainLock's enforcement rejects the bad-payee block, and the restart retries it.
    let d_cl = by_label("ChainLock of the bad block");
    assert_eq!(reject_why(d_cl), ["bad-cb-payee"]);
    let d_restart = by_label("checkpoint: lock removed, block rejected");
    assert!(
        reject_why(d_restart).iter().any(|why| why == "bad-cb-payee"),
        "{:?}",
        reject_why(d_restart)
    );
    // (c'): B's block is disconnected before X connects.
    let kinds: Vec<_> =
        lk1b_is::attempts(&by_label("ChainLock of X").records).iter().map(|a| a.kind).collect();
    assert_eq!(kinds, [dash_statedigest::Kind::Disconnect, dash_statedigest::Kind::Connect]);
    // (d)'s ChainLock timer left retries, so the periodic rule ran on real data.
    let raw: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(lk1b_is::data_dir().join("main.json")).unwrap(),
    )
    .unwrap();
    let periodic: Vec<_> = raw["steps"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s.get("periodic").is_some())
        .map(|s| s["scenario"].as_str().unwrap())
        .collect();
    assert!(!periodic.is_empty() && periodic.iter().all(|s| *s == "d"), "{periodic:?}");
    assert_eq!(drive_fixture(&main, echo), vec![]);

    let rot = lk1b_is::capture("rot");
    assert_eq!(rot.run, "rot");
    assert_eq!(rot.steps.iter().filter(|s| matches!(s.op, Op::IsLock { .. })).count(), 3);
    assert!(rot.steps.last().unwrap().checkpoint.is_some());
    assert_eq!(drive_fixture(&rot, echo), vec![]);
}

#[test]
#[cfg(feature = "test-support")]
fn core_differential_main_and_rot() {
    for run in ["main", "rot"] {
        let fixture = lk1b_is::capture(run);
        assert_eq!(fixture.run, run);
        let mut side = node_sut_side(&fixture).unwrap_or_else(|e| panic!("{run}: {e}"));
        let mismatches = lk1b_is::drive(&fixture, side.as_mut()).unwrap_or_else(|e| panic!("{e}"));
        assert!(mismatches.is_empty(), "{run}: {mismatches:#?}");
    }
}
