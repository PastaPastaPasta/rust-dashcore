//! L3 (CH3a a10): the non-evo mutation harness against a real dashd v24, with V and the shim
//! beside it. Test-only, behind `l3`.
//!
//! A run resolves its dashd (plan §3.1), runs the main legs `par1` and `par4` in parallel
//! threads ([`leg`]), then replays each on the shim, one session at a time ([`replay`]), and
//! writes the report ([`report`]).

// A failed expectation of the harness's own construction is a harness bug: panicking is the
// report.
#![allow(clippy::expect_used)]

pub mod dashd;
pub mod evo;
pub mod inventory;
pub mod ledger;
pub mod leg;
pub mod model;
pub mod planes;
pub mod replay;
pub mod report;
pub mod trial;
pub mod version;
#[cfg(test)]
mod witness;

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::chain::mutate::Par;

/// What a run runs, from the environment the test and the CLI share.
#[derive(Clone, Debug)]
pub struct Config {
    /// `L3_SET`: `tier1` (a10-4's only set; the default).
    pub set: String,
    /// `L3_REPORT`: where the report goes, if anywhere.
    pub report: Option<PathBuf>,
    /// `L3_ONLY`: one operator or case id (plan §3.2's filtered run).
    pub only: Option<String>,
    /// The main legs to run (both, from the environment).
    pub legs: Vec<Par>,
    /// `L3_RECORD`: a directory for each leg's recorded planes (`<leg>.json`), the witnesses'
    /// fixture source.
    pub record: Option<PathBuf>,
}

impl Config {
    pub fn from_env() -> Result<Config, String> {
        let set = std::env::var("L3_SET").unwrap_or_else(|_| "tier1".into());
        if set != "tier1" {
            return Err(format!("L3_SET={set}: a10-4 has tier1 only"));
        }
        Ok(Config {
            set,
            report: std::env::var_os("L3_REPORT").map(PathBuf::from),
            only: std::env::var("L3_ONLY").ok().filter(|o| !o.is_empty()),
            legs: vec![Par::One, Par::Four],
            record: std::env::var_os("L3_RECORD").map(PathBuf::from),
        })
    }
}

/// Run `config` from the repository at `repo`, in `dir` (each leg's datadirs under it). `Err`
/// for a run that could not start (the version checks, the inventory, the report's write); a
/// leg that failed is in the report, with `done` false.
pub fn run(config: &Config, repo: &Path, dir: &Path) -> Result<(Value, bool), String> {
    let dashd = version::resolve(repo, dir)?;
    let inventory = inventory::Inventory::load()?;
    if inventory.provenance.commit != dashd.commit || inventory.provenance.tag != dashd.tag {
        return Err(format!(
            "the inventory is for {} {}, the run's dashd for {} {}",
            inventory.provenance.tag, inventory.provenance.commit, dashd.tag, dashd.commit
        ));
    }
    let only = config.only.as_deref();
    let logs: Vec<leg::LegLog> = std::thread::scope(|s| {
        let handles: Vec<_> = config
            .legs
            .iter()
            .map(|&par| {
                let (dashd, inventory) = (&dashd, &inventory);
                s.spawn(move || {
                    let config = leg::LegConfig {
                        dashd,
                        par,
                        inventory,
                        only,
                        record: config.record.is_some(),
                    };
                    leg::run(&config, dir)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().expect("a leg's thread")).collect()
    });
    let replays: Vec<replay::Replayed> = logs.iter().map(replay::replay).collect();
    let legs: Vec<(Value, bool)> =
        logs.iter().zip(&replays).map(|(log, r)| report::leg(log, r, only)).collect();
    let head = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_default();
    if let Some(record) = &config.record {
        for (log, r) in logs.iter().zip(&replays) {
            let snapshots: Vec<Value> = log
                .recorded
                .iter()
                .map(|s| {
                    let mut s = s.clone();
                    let op = s["op"].as_u64().map(|i| i as usize);
                    s["core_post"] = op
                        .and_then(|i| r.posts.get(&i))
                        .map_or(Value::Null, |p| serde_json::from_str(p).unwrap_or(Value::Null));
                    s
                })
                .collect();
            let file = serde_json::json!({
                "provenance": {
                    "note": "recorded by L3_RECORD: dashd's replies (dashd below) and Core's posts (the shim's replay at the pin) after each trial op; V's reads under v",
                    "dashd": {"version": dashd.version, "tag": dashd.tag, "commit": dashd.commit, "sha256": dashd.sha256},
                    "rdc_head": head,
                    "leg": log.name,
                    "chain_args": log.chain_args,
                },
                "snapshots": snapshots,
            });
            let path = record.join(format!("{}.json", log.name));
            let text = serde_json::to_string_pretty(&file).expect("a JSON value");
            std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
        }
    }
    let done = legs.iter().all(|(_, d)| *d);
    let value = report::run(&dashd, &head, &config.set, &legs);
    if let Some(path) = &config.report {
        let text = serde_json::to_string_pretty(&value).expect("a JSON value");
        std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok((value, done))
}
