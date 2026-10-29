//! `diff_chain`'s Core half (O2b-3a, CoreOnly): every seed program on S0 and S1 strictly (no
//! unexpected reply, no tie, a same-process replay byte-identical and equal plane by plane
//! through `chain::compare`), the census over the seeds covering the §6.5 rows reachable without
//! Rust, every mutation operator's reject from Core (the expectation check is the assertion),
//! the Core halves of the witnesses W-P3a and W-P3b, and `chain::compare` naming the plane or
//! post field of a difference injected for each of its checks.
//!
//! Output: one line per schedule with the census counts (no input).

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeSet;

use dash_oracle_sys::{ChainReport, Pin, RawReport};
use dash_statedigest::{Origin, Record};
use node_l1::chain::Runner;
use node_l1::chain::census::{Census, Totals};
use node_l1::chain::compare::compare;
use node_l1::chain::core::Schedule;
use node_l1::chain::intent::Program;
use node_l1::chain::mutate::OPERATORS;
use node_l1::chain::run::{End, Run};
use node_l1::chain::step::Step;

/// `BLOCK_FAILED_VALID` (chain.h).
const BLOCK_FAILED_VALID: u32 = 32;

fn seeds() -> Vec<(String, Program)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/chain_seeds");
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .expect("tests/chain_seeds")
        .map(|e| e.expect("entry").path())
        .filter(|p| p.extension().is_some_and(|e| e == "prog"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|p| {
            let name = p.file_stem().expect("stem").to_string_lossy().into_owned();
            let text = std::fs::read_to_string(&p).expect("read");
            let program = Program::parse(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
            (name, program)
        })
        .collect()
}

fn steps(raws: &[RawReport]) -> Vec<Step> {
    raws.iter().map(|raw| ChainReport::decode(raw.clone()).into()).collect()
}

/// Run `program` twice: it must end [`End::Done`] and replay byte-identically. Then each step
/// pair goes through the comparator 3b uses: a reflexivity check (equal steps compare equal,
/// decoding included), not an independent replay check, which the byte comparison is.
fn strict(runner: &mut Runner, name: &str, program: &Program) -> Run {
    let run = runner.run(program).unwrap_or_else(|e| panic!("{name}: shim fault {}", e.0));
    assert_eq!(run.end, End::Done, "{name} on {}", runner.schedule.name());
    let again = runner.run(program).unwrap_or_else(|e| panic!("{name}: shim fault {}", e.0));
    assert!(again.raws == run.raws, "{name} on {}: the replay differs", runner.schedule.name());
    for (i, (a, b)) in steps(&run.raws).iter().zip(steps(&again.raws)).enumerate() {
        if let Err(d) = compare(a, &b, &[]) {
            panic!("{name} on {}: step {i}'s {} differs", runner.schedule.name(), d.field);
        }
    }
    run
}

/// `compare` names the plane, or the post field, of each difference injected into one of the
/// seeds' steps: one injection per check.
fn injected(steps: &[Step]) {
    let named = |change: &dyn Fn(&mut Step) -> bool| -> Option<&'static str> {
        steps.iter().find_map(|step| {
            let mut other = step.clone();
            change(&mut other).then(|| compare(step, &other, &[]).expect_err("a difference").field)
        })
    };
    // An outcome record of an origin the plane compares (`verifydb`'s are counted apart).
    let compared = |r: &Record| matches!(r, Record::Outcome(o) if o.id.org != Origin::Verifydb);
    let outcome = |s: &mut Step| match s.records.iter().rposition(compared) {
        Some(i) => {
            s.records.remove(i);
            true
        }
        None => false,
    };
    let event = |s: &mut Step| s.events.pop().is_some();
    let reply = |s: &mut Step| {
        s.reply.outcome = Ok(serde_json::Value::from("injected"));
        true
    };
    let tip = |s: &mut Step| {
        s.post.tip = None;
        true
    };
    let utxo = |s: &mut Step| s.post.utxo.take().is_some();
    let tips = |s: &mut Step| s.post.tips.take().is_some();
    let ibd = |s: &mut Step| {
        s.post.ibd = s.post.ibd.map(|b| !b);
        s.post.ibd.is_some()
    };
    assert_eq!(named(&reply), Some("reply"));
    assert_eq!(named(&outcome), Some("records"));
    assert_eq!(named(&event), Some("events"));
    assert_eq!(named(&tip), Some("post.tip"));
    assert_eq!(named(&utxo), Some("post.utxo"));
    assert_eq!(named(&tips), Some("post.tips"));
    assert_eq!(named(&ibd), Some("post.ibd"));

    let attempt =
        |s: &mut Step| match s.records.iter().rposition(|r| matches!(r, Record::Attempt(_))) {
            Some(i) => {
                s.records.remove(i);
                true
            }
            None => false,
        };
    let sparse = |s: &mut Step| match s.records.iter().rposition(|r| matches!(r, Record::Sparse(_)))
    {
        Some(i) => {
            s.records.remove(i);
            true
        }
        None => false,
    };
    // The seeds may record no sparse record (none does at 3a/3h): that check is then left
    // uninjected, and the run says so.
    let has = |p: fn(&Record) -> bool| steps.iter().any(|s| s.records.iter().any(p));
    if has(|r| matches!(r, Record::Attempt(_))) {
        assert_eq!(named(&attempt), Some("attempts"));
    } else {
        eprintln!("l1_chain: the seeds record no attempt: the attempts check is not injected");
    }
    if has(|r| matches!(r, Record::Sparse(_))) {
        assert_eq!(named(&sparse), Some("sparse records"));
    } else {
        eprintln!("l1_chain: the seeds record no sparse record: that check is not injected");
    }

    let down = |s: &mut Step| {
        s.post.down = !s.post.down;
        true
    };
    let flush_delta = |s: &mut Step| {
        s.post.flush_delta.full.insert("injected".into(), 1);
        true
    };
    let persisted = |s: &mut Step| s.post.persisted.take().is_some();
    let best_header = |s: &mut Step| {
        s.post.best_header = match s.post.best_header.take() {
            Some(_) => None,
            None => Some(None),
        };
        true
    };
    let index = |s: &mut Step| s.post.index.take().is_some();
    let flushes = |s: &mut Step| s.post.flushes.take().is_some();
    let best_cl = |s: &mut Step| {
        s.post.best_cl = match s.post.best_cl.take() {
            Some(_) => None,
            None => Some(None),
        };
        true
    };
    let sporks = |s: &mut Step| s.post.sporks.take().is_some();
    let deployments = |s: &mut Step| s.post.deployments.take().is_some();
    assert_eq!(named(&down), Some("post.down"));
    assert_eq!(named(&flush_delta), Some("post.flush_delta"));
    assert_eq!(named(&persisted), Some("post.persisted"));
    assert_eq!(named(&best_header), Some("post.best_header"));
    assert_eq!(named(&index), Some("post.index"));
    assert_eq!(named(&flushes), Some("post.flushes"));
    assert_eq!(named(&best_cl), Some("post.best_cl"));
    assert_eq!(named(&sporks), Some("post.sporks"));
    assert_eq!(named(&deployments), Some("post.deployments"));
}

fn one_reject(census: &Census) -> String {
    let rejects: Vec<&String> = census.rejects.keys().collect();
    assert_eq!(rejects.len(), 1, "one reject expected");
    rejects[0].clone()
}

fn schedule(schedule: Schedule) {
    let mut runner = Runner::new(Pin::V24_0, schedule).unwrap_or_else(|e| panic!("start: {}", e.0));
    let mut totals = Totals::default();
    let mut all_steps = Vec::new();
    for (name, program) in seeds() {
        let run = strict(&mut runner, &name, &program);
        all_steps.extend(steps(&run.raws));
        if name == "w_p3b" {
            // The restart's persisted image: the target and its two descendants stored failed.
            let report = ChainReport::decode(run.raws.last().expect("steps").clone());
            let failed = report.post.persisted.expect("a restart").failed;
            let n = failed.iter().filter(|(_, bits)| bits & BLOCK_FAILED_VALID != 0).count();
            assert_eq!(n, 3, "W-P3b: blocks stored with BLOCK_FAILED_VALID");
        }
        if name == "w_p3a" {
            let reason = one_reject(&run.census);
            if schedule == Schedule::S1 {
                assert!(reason.starts_with("bad-txns-nonfinal"), "W-P3a on S1: {reason}");
            } else {
                assert!(!reason.starts_with("bad-txns-nonfinal"), "W-P3a on S0: {reason}");
            }
        }
        totals.input(run.census, None);
    }

    let c = &totals.census;
    eprintln!(
        "l1_chain {}: rows {:?}, ops {:?}, rejects {:?}, avoided {:?}",
        schedule.name(),
        c.rows,
        c.ops,
        c.rejects,
        c.avoided
    );
    let mut rows: BTreeSet<&str> = [
        "reorg_3",
        "kill_with_unflushed_entries",
        "invalidate_undone_by_restart",
        "cl_before_header",
        "refused_load",
        "reconsider_ignore_cl_conflict",
        "precious_switch",
        "ibd_flip_settime",
        "testdummy_active",
    ]
    .into();
    if schedule == Schedule::S1 {
        rows.insert("multi_failure_s1");
    }
    let missing: Vec<_> = rows.iter().filter(|r| !c.rows.contains_key(*r)).collect();
    assert!(missing.is_empty(), "{}: census rows not reached: {missing:?}", schedule.name());
    for kind in ["block", "header", "proposal", "invalidate", "reconsider", "precious"] {
        for class in ["ok", "reject"] {
            assert!(c.ops.contains_key(&(kind, class)), "{}: no {kind} {class}", schedule.name());
        }
    }
    for kind in ["cl_record", "cl_enforce", "flush", "restart", "settime", "activate"] {
        assert!(c.ops.keys().any(|(k, _)| *k == kind), "{}: no {kind}", schedule.name());
    }
    for reject in ["bad-chainlock", "bad-prevblk-chainlock", "prev-blk-not-found"] {
        assert!(
            c.rejects.keys().any(|r| r.starts_with(reject)),
            "{}: no {reject}",
            schedule.name()
        );
    }
    assert!(c.avoided.contains_key("spork_not_admitted"), "spork intents are counted no-ops (D34)");

    // Every operator reaches a reject (the run's expectation check is the assertion: the reject
    // must be one the operator lists).
    let mut reached = Vec::new();
    for (i, op) in OPERATORS.iter().enumerate() {
        let text = format!("extend main 1 tx=spends\nmutate #{i}\nextend main 1\n");
        let program = Program::parse(&text).expect("parses");
        let run =
            runner.run(&program).unwrap_or_else(|e| panic!("{}: shim fault {}", op.name, e.0));
        assert_eq!(run.end, End::Done, "operator {} on {}", op.name, schedule.name());
        reached.push(format!("{}={}", op.name, one_reject(&run.census)));
    }
    eprintln!("l1_chain {}: operators [{}]", schedule.name(), reached.join(", "));

    injected(&all_steps);
}

#[test]
fn s0_seeds_operators_and_witnesses() {
    schedule(Schedule::S0);
}

#[test]
fn s1_seeds_operators_and_witnesses() {
    schedule(Schedule::S1);
}
