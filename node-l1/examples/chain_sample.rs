//! A generated sample through `diff_chain`'s Core half (O2b-3a measurements): `count` inputs
//! of random bytes (splitmix64 from `seed`, 16 to 400 bytes) through [`Runner`], the ones
//! [`replayed`] picks run twice, as in the fuzz target. Prints the census with the tie-discard
//! rate (D30, gated at 1%), and inputs per CPU second of this process.
//!
//! `cargo run --release -p node-l1 --features oracle --example chain_sample -- S0 10000 1`
//! (`CHAIN_SAMPLE_LAST=FILE` keeps the program about to run in FILE, for a crash's triage;
//! `CHAIN_SAMPLE_UNEXPECTED=DIR` keeps each input that ended on an unexpected reply, and
//! `CHAIN_SAMPLE_DISCARDED=DIR` each one a tie discarded).

use dash_oracle_sys::Pin;
use node_l1::chain::Runner;
use node_l1::chain::core::Schedule;
use node_l1::chain::intent::Program;
use node_l1::chain::run::{End, replayed, replays};

/// This process's user + system CPU seconds (`/proc/self/stat`, 100 ticks a second).
fn cpu_seconds() -> f64 {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let fields: Vec<&str> =
        stat.rsplit_once(')').map_or("", |(_, s)| s).split_whitespace().collect();
    let tick = |i: usize| fields.get(i).and_then(|f| f.parse::<f64>().ok()).unwrap_or(0.0);
    (tick(11) + tick(12)) / 100.0
}

/// The schedule an argument names: S1, else S0.
fn schedule_arg(arg: Option<&String>) -> Schedule {
    if arg.map(String::as_str) == Some("S1") {
        Schedule::S1
    } else {
        Schedule::S0
    }
}

/// `--trace FILE [S0|S1]`: one program's steps, each op kind with Core's reply and tip.
fn trace(path: &str, schedule: Schedule) {
    let program = Program::parse(&std::fs::read_to_string(path).expect("read")).expect("parse");
    let mut runner = Runner::new(Pin::V24_0, schedule).expect("session");
    let run = runner.run(&program).expect("shim");
    for (kind, raw) in run.kinds.iter().zip(&run.raws) {
        let post: serde_json::Value = serde_json::from_slice(&raw.post).expect("post");
        let events: serde_json::Value = serde_json::from_slice(&raw.events).expect("events");
        println!(
            "{kind:<11} {} tip={} tie={} events={}",
            String::from_utf8_lossy(&raw.reply),
            post["tip"],
            post["tie"],
            events.as_array().map_or(0, Vec::len)
        );
        if std::env::var_os("TRACE_TIPS").is_some() {
            println!("            tips={} best_cl={}", post["tips"], post["best_cl"]);
        }
    }
    println!("end {:?}", run.end);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--trace") {
        return trace(&args[2], schedule_arg(args.get(3)));
    }
    let schedule = schedule_arg(args.get(1));
    let count: u64 = args.get(2).and_then(|a| a.parse().ok()).unwrap_or(1000);
    let mut x: u64 = args.get(3).and_then(|a| a.parse().ok()).unwrap_or(1);
    let mut next = move || {
        x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = x;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    };
    let mut runner = Runner::new(Pin::V24_0, schedule).expect("session");
    let start = cpu_seconds();
    for n in 1..=count {
        let len = 16 + (next() % 385) as usize;
        let bytes: Vec<u8> = (0..len).map(|_| next() as u8).collect();
        let program = Program::decode(&bytes);
        if let Some(last) = std::env::var_os("CHAIN_SAMPLE_LAST") {
            // The program about to run, for a crash's triage.
            std::fs::write(last, format!("# input {n}\n{program}")).expect("CHAIN_SAMPLE_LAST");
        }
        let run = runner.run(&program).expect("shim");
        // Each input that ended on an unexpected reply or a tie, for the expectations' and the
        // tie model's triage.
        let kept = match &run.end {
            End::Unexpected(what) => Some(("CHAIN_SAMPLE_UNEXPECTED", what.clone())),
            End::Tainted(op, sites) => Some(("CHAIN_SAMPLE_DISCARDED", format!("{op} {sites:?}"))),
            End::Done => None,
            End::Diverged(_) | End::Unsupported(_) => unreachable!("a Core-only runner"),
        };
        if let Some((var, what)) = kept
            && let Some(dir) = std::env::var_os(var)
        {
            let path = std::path::Path::new(&dir).join(format!("input-{n}.prog"));
            std::fs::write(path, format!("# {what}\n{program}")).expect("a kept input");
        }
        if replayed(&bytes) {
            runner.totals.replays += 1;
            let again = runner.run(&program).expect("shim");
            if !replays(&run, &again) {
                runner.totals.replay_mismatches += 1;
            }
        }
        runner.totals.input(run.census, run.end.discard());
    }
    let cpu = cpu_seconds() - start;
    let mut stats = runner.totals.to_json();
    stats["schedule"] = schedule.name().into();
    stats["cpu_seconds"] = cpu.into();
    stats["inputs_per_cpu_second"] = (count as f64 / cpu.max(1e-9)).into();
    println!("{stats:#}");
}
