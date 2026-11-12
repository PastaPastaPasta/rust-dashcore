//! The spork experiment (O2b-4 PLAN §5.4): three systems (Core's session, `NodeSut`, stock
//! dashd) × two fresh trials (KILL, CLEAN) × two checks. From the image with spork 19 on (value
//! 0), `SPORK_19_CHAINLOCKS_ENABLED` is set to 4070908800 and must read off; after a KILL it reads
//! on again (the only `sporks.dat` writer is `~CSporkManager`, reached through Shutdown's
//! `sporkman.reset()`), after a CLEAN restart off (the cache loads at init).
// Core: src/spork.cpp:46-50, src/init.cpp:416, src/init.cpp:1959-1962 at v24.0.0

use std::path::Path;

use dash_oracle_sys::{ChainOp, Pin, RestartKind, SporkState};
use serde_json::Value;

use super::{Binary, Dashd, copy_tree, opmap, private_dir};
use crate::chain::core::{CoreSut, SPORK_19, SPORK_OFF, Schedule, T_RECENT};
use crate::chain::node::NodeSide;

pub const SYSTEMS: [&str; 3] = ["session", "nodesut", "dashd"];
pub const TRIALS: [RestartKind; 2] = [RestartKind::Kill, RestartKind::Clean];
pub const CHECKS: [&str; 2] = ["before_restart", "after_restart"];

pub fn trial_name(kind: RestartKind) -> &'static str {
    match kind {
        RestartKind::Kill => "kill",
        RestartKind::Clean => "clean",
    }
}

/// Spork 19 as each check expects it: off before either restart; on after a KILL, off after a
/// CLEAN one.
pub fn expected(trial: RestartKind, check: &str) -> SporkState {
    match (trial, check) {
        (RestartKind::Kill, "after_restart") => SporkState(SPORK_19, 0, true),
        _ => SporkState(SPORK_19, SPORK_OFF, false),
    }
}

/// One assertion: what the system showed for spork 19 (`None` where it showed no spork 19).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub system: &'static str,
    pub trial: &'static str,
    pub check: &'static str,
    pub expected: SporkState,
    pub observed: Option<SporkState>,
}

impl Row {
    fn new(
        system: &'static str,
        trial: RestartKind,
        check: &'static str,
        sporks: &[SporkState],
    ) -> Row {
        Row {
            system,
            trial: trial_name(trial),
            check,
            expected: expected(trial, check),
            observed: sporks.iter().find(|s| s.0 == SPORK_19).cloned(),
        }
    }

    pub fn passed(&self) -> bool {
        self.observed.as_ref() == Some(&self.expected)
    }
}

/// The experiment's verdict: every one of the twelve rows present once and passing. A missing
/// row (a skipped system or trial) fails as a wrong one does.
pub fn verdict(rows: &[Row]) -> Result<(), String> {
    let mut failures = Vec::new();
    for system in SYSTEMS {
        for trial in TRIALS {
            for check in CHECKS {
                let found: Vec<&Row> = rows
                    .iter()
                    .filter(|r| {
                        r.system == system && r.trial == trial_name(trial) && r.check == check
                    })
                    .collect();
                match found.as_slice() {
                    [row] if row.passed() => {}
                    [row] => failures.push(format!(
                        "{system}/{}/{check}: {:?}, expected {:?}",
                        row.trial, row.observed, row.expected
                    )),
                    [] => failures.push(format!("{system}/{}/{check}: missing", trial_name(trial))),
                    _ => failures.push(format!("{system}/{}/{check}: repeated", trial_name(trial))),
                }
            }
        }
    }
    if rows.len() != SYSTEMS.len() * TRIALS.len() * CHECKS.len() {
        failures.push(format!("{} rows, not 12", rows.len()));
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

fn turn_off() -> ChainOp {
    ChainOp::Spork {
        id: SPORK_19,
        value: SPORK_OFF,
    }
}

/// The session's and `NodeSut`'s eight rows on S0, each trial from a fresh restore. Opens the
/// process's oracle session and closes it on return (the node first, D8's check needs the oracle
/// free), so it runs before any runner.
pub fn session_rows(dir: &Path) -> Result<Vec<Row>, String> {
    private_dir(dir)?;
    let (mut node, _) = NodeSide::start(Pin::V24_0, Schedule::S0, dir)?;
    let (mut core, _) = CoreSut::start(Pin::V24_0, Schedule::S0)?;
    let mut rows = Vec::new();
    for trial in TRIALS {
        let restored = core.restore(T_RECENT)?;
        node.restore(T_RECENT)?;
        if restored.tainted {
            return Err("the session's restore is tie-tainted".into());
        }
        for (check, op) in CHECKS.into_iter().zip([turn_off(), ChainOp::Restart(trial)]) {
            let c = core.step(&op)?;
            let n = node.step(&op).map_err(|u| format!("the node: {u:?}"))?;
            if c.tainted {
                return Err(format!("the session's {} is tie-tainted", trial_name(trial)));
            }
            let sporks = |post: &dash_oracle_sys::Post| post.sporks.clone().unwrap_or_default();
            rows.push(Row::new("session", trial, check, &sporks(&c.step.post)));
            rows.push(Row::new("nodesut", trial, check, &sporks(&n.step.post)));
        }
    }
    Ok(rows)
}

/// dashd's four rows: each trial on a fresh copy of `base` (S0's) at `dir/spork-<trial>/`,
/// removed after.
pub fn dashd_rows(binary: &Binary, base: &Path, dir: &Path) -> Result<Vec<Row>, String> {
    let mut rows = Vec::new();
    for trial in TRIALS {
        let work = dir.join(format!("spork-{}", trial_name(trial)));
        copy_tree(base, &work)?;
        let mut node = Dashd::new(binary, &work, Schedule::S0, T_RECENT);
        let result = dashd_trial(&mut node, trial, &mut rows);
        let stopped = node.stop();
        std::fs::remove_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
        result?;
        stopped?;
    }
    Ok(rows)
}

fn dashd_trial(
    node: &mut Dashd<'_>,
    trial: RestartKind,
    rows: &mut Vec<Row>,
) -> Result<(), String> {
    if let Some(error) = node.start()? {
        return Err(format!("the spork trial's start was refused: {error:?}"));
    }
    let before = node.planes()?.sporks;
    if before.iter().find(|s| s.0 == SPORK_19) != Some(&SporkState(SPORK_19, 0, true)) {
        return Err(format!("spork 19 is not on in the base: {before:?}"));
    }
    let off = turn_off();
    let answer = node.run(&off, &opmap::map(&off, true))?;
    if answer != Ok(Value::String("success".into())) {
        return Err(format!("sporkupdate: {answer:?}"));
    }
    rows.push(Row::new("dashd", trial, CHECKS[0], &node.planes()?.sporks));
    let restart = ChainOp::Restart(trial);
    let answer = node.run(&restart, &opmap::map(&restart, true))?;
    if super::compare::refused(&answer) {
        return Err(format!("the {} restart was refused: {answer:?}", trial_name(trial)));
    }
    rows.push(Row::new("dashd", trial, CHECKS[1], &node.planes()?.sporks));
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use dash_oracle_sys::SporkState;

    use super::{CHECKS, Row, SYSTEMS, TRIALS, expected, trial_name, verdict};
    use crate::chain::core::SPORK_19;

    fn all() -> Vec<Row> {
        let mut rows = Vec::new();
        for system in SYSTEMS {
            for trial in TRIALS {
                for check in CHECKS {
                    rows.push(Row {
                        system,
                        trial: trial_name(trial),
                        check,
                        expected: expected(trial, check),
                        observed: Some(expected(trial, check)),
                    });
                }
            }
        }
        rows
    }

    /// The verdict needs all twelve rows: a missing, repeated or wrong one fails it.
    #[test]
    fn the_spork_verdict_needs_every_row() {
        assert_eq!(verdict(&all()), Ok(()));
        let mut missing = all();
        missing.retain(|r| !(r.system == "dashd" && r.trial == "clean"));
        assert!(verdict(&missing).unwrap_err().contains("dashd/clean/before_restart: missing"));
        let mut repeated = all();
        repeated.push(repeated[0].clone());
        assert!(verdict(&repeated).unwrap_err().contains("repeated"));
        let mut wrong = all();
        wrong[4].observed = Some(SporkState(SPORK_19, 0, true));
        assert!(verdict(&wrong).is_err());
        let mut absent = all();
        absent[0].observed = None;
        assert!(verdict(&absent).is_err());
    }
}
