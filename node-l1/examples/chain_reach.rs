//! The reach report (O2b-4 4e1 DESIGN §5.3): every seed program of each profile's chain target
//! run in Core's session alone (`Mode::CoreOnly`), with its census, and G-ENC-4's append
//! property on the grown bank. `l3-census.py o2b4-check --reach` then requires each required
//! cell's seed to have credited its key on its profile. A seed's census is credited only when its
//! run ends done (a run's other ends are discarded or counted unexpected, `End::discard`); one
//! that does not is recorded with its end alone and fails the run (OD-L3-171 (2)).
//!
//! A profile's seeds are the programs in `tests/chain_seeds/*.prog`, `chain-v2/*.prog` and
//! `<target>/*.prog`, named by their path below `tests/chain_seeds`.
//!
//! `cargo run -p node-l1 --features oracle --example chain_reach -- --head <sha> --out <path>`

use std::path::{Path, PathBuf};

use dash_oracle_sys::Pin;
use node_l1::chain::Runner;
use node_l1::chain::census::Totals;
use node_l1::chain::core::Schedule;
use node_l1::chain::generate::g_enc4_append_only;
use node_l1::chain::intent::{Program, chain_encodings};
use node_l1::chain::mutate::BANK;
use node_l1::chain::run::End;
use serde_json::{Map, Value, json};

/// The profiles with a chain target on this tree: (profile, target, schedule).
const PROFILES: &[(&str, &str, Schedule)] = &[
    ("S0", "diff_chain_s0", Schedule::S0),
    ("S1", "diff_chain_par", Schedule::S1),
    ("S2", "diff_chain_av", Schedule::S2),
    ("S0p", "diff_chain_prodflush", Schedule::S0p),
];

/// The `.prog` files in `dir`, sorted, named `prefix` + file name.
fn progs(dir: &Path, prefix: &str) -> std::io::Result<Vec<(String, PathBuf)>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "prog") {
            let name = path.file_name().expect("a file name").to_string_lossy().into_owned();
            out.push((format!("{prefix}{name}"), path));
        }
    }
    out.sort();
    Ok(out)
}

fn arg(args: &[String], flag: &str) -> String {
    let i = args.iter().position(|a| a == flag).unwrap_or_else(|| panic!("{flag} is required"));
    args.get(i + 1).unwrap_or_else(|| panic!("{flag} needs a value")).clone()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let (head, out) = (arg(&args, "--head"), arg(&args, "--out"));
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/chain_seeds");
    let shared = [progs(&root, "")?, progs(&root.join("chain-v2"), "chain-v2/")?].concat();
    let mut every = Vec::new();
    let mut profiles = Map::new();
    let mut targets = Vec::new();
    let mut faults = 0;
    let mut not_done = 0;
    for &(profile, target, schedule) in PROFILES {
        let own = progs(&root.join(target), &format!("{target}/"))?;
        let mut runner =
            Runner::new(Pin::V24_0, schedule).map_err(|e| format!("start: {}", e.0))?;
        // The target's encoding, so its generator admits what the target's does (a `chain-v1`
        // runner would drop a `chain-v2` program's spork intents).
        runner.encoding = chain_encodings()
            .into_iter()
            .find(|&(n, _)| n == target)
            .ok_or_else(|| format!("{target} is not in node-l1/chain-encodings.toml"))?
            .1;
        let mut seeds = Map::new();
        for (name, path) in shared.iter().chain(&own) {
            let program = Program::parse(&std::fs::read_to_string(path)?)
                .map_err(|e| format!("{name}: {e}"))?;
            every.push(program.encode_v2());
            let census = match runner.run(&program) {
                Ok(run) if run.end == End::Done => {
                    let mut totals = Totals::default();
                    totals.input(run.census, None);
                    let mut census = totals.to_json();
                    census["end"] = format!("{:?}", run.end).into();
                    census
                }
                Ok(run) => {
                    not_done += 1;
                    eprintln!("chain_reach {profile}: {name} ends {:?}", run.end);
                    json!({"end": format!("{:?}", run.end)})
                }
                Err(fault) => {
                    faults += 1;
                    json!({"shim_fault": fault.0.to_string()})
                }
            };
            seeds.insert(name.clone(), census);
        }
        eprintln!("chain_reach {profile} ({target}): {} seeds", seeds.len());
        profiles.insert(profile.into(), Value::Object(seeds));
        targets.push(target);
    }
    let g_enc4 = match g_enc4_append_only(every) {
        Ok(()) => "pass".to_owned(),
        Err(e) => format!("fail: {e}"),
    };
    let report = json!({
        "schema": "o2b4-reach/1",
        "head": head,
        "bank": BANK.iter().map(|o| o.name).collect::<Vec<_>>(),
        "g_enc4": g_enc4,
        "targets": targets,
        "profiles": profiles,
    });
    std::fs::write(&out, serde_json::to_string_pretty(&report)? + "\n")?;
    eprintln!("chain_reach: g_enc4 {g_enc4}, {faults} shim faults, {not_done} not done, to {out}");
    if faults > 0 || not_done > 0 || g_enc4 != "pass" {
        return Err("the reach run failed".into());
    }
    Ok(())
}
