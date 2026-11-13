use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{BufReader, Write};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::Path;

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tempfile::NamedTempFile;

use super::alarm::{Alarm, AlarmKey, Class};
use super::settle::{ChainLock, Settle};
use super::side::SideState;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub h: i64,
    pub hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settled {
    pub hash: String,
    pub fingerprint: Vec<u8>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Evidence {
    pub h: i64,
    pub connected: [bool; 2],
    pub invalid: [bool; 2],
    pub tip_reject: [bool; 2],
    pub rejected: [bool; 2],
    pub proposal: [bool; 2],
    pub rewound: bool,
}

impl Evidence {
    pub(super) fn contradicts(&self) -> bool {
        (self.connected[0] && self.invalid[1]) || (self.connected[1] && self.invalid[0])
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub checkpoint: Option<Checkpoint>,
    pub lag_r: i64,
    pub lag_l: i64,
    pub clock_start: u64,
    pub crit: usize,
    pub warn: usize,
    pub last: Option<AlarmKey>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    pub sides: [SideState; 2],
    pub checkpoint: Option<Checkpoint>,
    #[serde(default)]
    pub settled: BTreeMap<i64, Settled>,
    pub evidence: BTreeMap<String, Evidence>,
    pub emitted: BTreeSet<AlarmKey>,
    pub alarms: Vec<Alarm>,
    pub events: Vec<Value>,
    pub settle: Option<Settle>,
    pub clock_start: u64,
    pub last_cl: Option<ChainLock>,
    pub cl_advanced_at: u64,
    pub cl_headers: i64,
    pub lag_since: Option<u64>,
    pub retry_after: u64,
    pub rpc_failures: u32,
}

impl State {
    pub fn read(path: &Path) -> Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        let state: Self = serde_json::from_reader(BufReader::new(File::open(path)?))?;
        ensure!(state.version == 1, "unsupported shadow state version");
        ensure!(
            state.emitted == state.alarms.iter().map(Alarm::key).collect(),
            "alarm journal disagrees with emitted keys"
        );
        Ok(Some(state))
    }

    pub fn status(&self, tips: [Option<i64>; 2]) -> Status {
        let [l, r] = tips;
        let delta = l.zip(r).map_or(0, |(l, r)| l - r);
        Status {
            checkpoint: self.checkpoint.clone(),
            lag_r: delta.max(0),
            lag_l: (-delta).max(0),
            clock_start: self.clock_start,
            crit: self.alarms.iter().filter(|a| matches!(a.class, Class::Crit(_))).count(),
            warn: self.alarms.iter().filter(|a| matches!(a.class, Class::Warn(_))).count(),
            last: self.alarms.last().map(Alarm::key),
        }
    }

    pub fn save(&self, out: &Path) -> Result<()> {
        fs::DirBuilder::new().recursive(true).mode(0o700).create(out)?;
        fs::set_permissions(out, fs::Permissions::from_mode(0o700))?;
        atomic(out, "state.json", &serde_json::to_vec(self)?)?;
        self.materialize(out)
    }

    pub fn materialize(&self, out: &Path) -> Result<()> {
        // State is the durable journal; replaying it repairs an interrupted output write.
        let mut alarms = Vec::new();
        for a in &self.alarms {
            serde_json::to_writer(&mut alarms, a)?;
            alarms.push(b'\n');
        }
        atomic(out, "alarms.jsonl", &alarms)?;
        let mut events = Vec::new();
        for e in &self.events {
            serde_json::to_writer(&mut events, e)?;
            events.push(b'\n');
        }
        atomic(out, "events.jsonl", &events)
    }
}

fn atomic(out: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    let mut file = NamedTempFile::new_in(out)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(out.join(name))?;
    File::open(out)?.sync_all()?;
    Ok(())
}
