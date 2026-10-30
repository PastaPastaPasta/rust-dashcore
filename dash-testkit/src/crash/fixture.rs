//! A fixture `contrib/node/crash/mkfixture.py` made: the oracle rows dashd recorded for every
//! block while it was the tip, the raw blocks, the delivery schedule and its tip trajectory, and
//! F01's rejected side block.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use serde_json::Value;

/// The `gettxoutsetinfo` fields C2 compares, per hash type.
pub const MUHASH_FIELDS: [&str; 4] = ["muhash", "txouts", "total_amount", "bogosize"];
pub const SERIALIZED_FIELDS: [&str; 1] = ["hash_serialized_2"];

/// One block's row: dashd's coin statistics with that block as its tip.
#[derive(Clone, Debug)]
pub struct Row {
    pub hash: String,
    pub height: u64,
    pub prev: Option<String>,
    pub fields: serde_json::Map<String, Value>,
}

/// F01: a side block dashd stores but rejects on connect, and its child that makes it connect.
#[derive(Clone, Debug)]
pub struct F01 {
    /// The block after which F and C are delivered (its equal-work sibling is F).
    pub after: String,
    pub f_hash: String,
    pub f_hex: String,
    pub c_hash: String,
    pub c_hex: String,
    /// dashd's verdict: its `submitblock` replies to F and to C, and C's `getchaintips` status.
    pub dashd_submit_f: Value,
    pub dashd_submit_c: Value,
    pub dashd_c_status: Value,
}

#[derive(Debug, Default)]
pub struct Fixture {
    pub dir: PathBuf,
    pub rows: HashMap<String, Row>,
    /// Raw blocks by hash, hex.
    pub blocks: HashMap<String, String>,
    /// The datadir (under `dir`) of the peer that serves the P2P part, and its tip.
    pub p2p_datadir: PathBuf,
    pub p2p_tip: String,
    /// Delivered by `submitblock` after the P2P part, in this order.
    pub submit: Vec<String>,
    pub final_tip: String,
    /// The mock time every node of a run uses.
    pub mocktime: i64,
    /// Every active tip a node passes through on the schedule.
    pub trajectory: Vec<String>,
    pub f01: Option<F01>,
    pub chain_args: Vec<String>,
}

fn bad(what: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_string())
}

fn json(path: &Path) -> io::Result<Value> {
    serde_json::from_reader(BufReader::new(File::open(path)?))
        .map_err(|e| bad(format!("{}: {e}", path.display())))
}

fn string(v: &Value, key: &str) -> io::Result<String> {
    v[key].as_str().map(str::to_owned).ok_or_else(|| bad(format!("no string {key:?}")))
}

impl Fixture {
    pub fn load(dir: &Path) -> io::Result<Fixture> {
        let mut rows = HashMap::new();
        let mut order = Vec::new();
        for line in BufReader::new(File::open(dir.join("rows.jsonl"))?).lines() {
            let v: Value = serde_json::from_str(&line?).map_err(bad)?;
            let mut fields = serde_json::Map::new();
            for key in MUHASH_FIELDS.iter().chain(&SERIALIZED_FIELDS) {
                fields.insert(key.to_string(), v[*key].clone());
            }
            let row = Row {
                hash: string(&v, "hash")?,
                height: v["height"].as_u64().ok_or_else(|| bad("no height"))?,
                prev: v["prev"].as_str().map(str::to_owned),
                fields,
            };
            order.push(row.hash.clone());
            rows.insert(row.hash.clone(), row);
        }
        // blocks.bin holds the blocks in the rows' (mining) order.
        let mut blocks = HashMap::new();
        let mut r = BufReader::new(File::open(dir.join("blocks.bin"))?);
        for hash in &order {
            let mut len = [0u8; 4];
            r.read_exact(&mut len)?;
            let mut raw = vec![0u8; u32::from_le_bytes(len) as usize];
            r.read_exact(&mut raw)?;
            blocks.insert(hash.clone(), hex(&raw));
        }
        if r.read(&mut [0u8])? != 0 {
            return Err(bad("blocks.bin holds more blocks than rows.jsonl"));
        }
        let schedule = json(&dir.join("schedule.json"))?;
        let submit = schedule["submit"]
            .as_array()
            .ok_or_else(|| bad("no submit list"))?
            .iter()
            .map(|h| h.as_str().map(str::to_owned).ok_or_else(|| bad("submit entry")))
            .collect::<io::Result<Vec<_>>>()?;
        let trajectory =
            serde_json::from_value(json(&dir.join("trajectory.json"))?).map_err(bad)?;
        let f01 = match fs::metadata(dir.join("f01.json")) {
            Ok(_) => {
                let f = json(&dir.join("f01.json"))?;
                Some(F01 {
                    after: string(&f, "next")?,
                    f_hash: string(&f["f"], "hash")?,
                    f_hex: string(&f["f"], "hex")?,
                    c_hash: string(&f["c"], "hash")?,
                    c_hex: string(&f["c"], "hex")?,
                    dashd_submit_f: f["verdict"]["submit_f"].clone(),
                    dashd_submit_c: f["verdict"]["submit_c"].clone(),
                    dashd_c_status: f["verdict"]["tips_after_c"]
                        .as_array()
                        .and_then(|t| t.iter().find(|x| x["hash"] == f["c"]["hash"]))
                        .map(|x| x["status"].clone())
                        .ok_or_else(|| bad("f01.json: no tip for C"))?,
                })
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        let provenance = json(&dir.join("provenance.json"))?;
        let chain_args = serde_json::from_value(provenance["chain_args"].clone()).map_err(bad)?;
        let fixture = Fixture {
            dir: dir.to_owned(),
            p2p_datadir: dir.join(string(&schedule["p2p"], "datadir")?),
            p2p_tip: string(&schedule["p2p"], "tip")?,
            submit,
            final_tip: string(&schedule, "final")?,
            mocktime: schedule["mocktime"].as_i64().ok_or_else(|| bad("no mocktime"))?,
            trajectory,
            f01,
            chain_args,
            rows,
            blocks,
        };
        for h in fixture.submit.iter().chain([&fixture.p2p_tip, &fixture.final_tip]) {
            if !fixture.blocks.contains_key(h) {
                return Err(bad(format!("schedule block {h} is not in the fixture")));
            }
        }
        Ok(fixture)
    }

    /// Where `stats` (a `gettxoutsetinfo` reply) differs from the row of its `bestblock`, for
    /// `fields`; `Ok` when it matches.
    pub fn check_stats(&self, stats: &Value, fields: &[&str]) -> Result<String, String> {
        let best = stats["bestblock"].as_str().ok_or("no bestblock")?;
        let row = self.rows.get(best).ok_or_else(|| format!("tip {best} is no fixture block"))?;
        for key in fields {
            if stats[*key] != row.fields[*key] {
                return Err(format!(
                    "{key} at {best} (height {}): node {} oracle {}",
                    row.height, stats[*key], row.fields[*key]
                ));
            }
        }
        Ok(best.to_owned())
    }

    /// The tip's position on the trajectory at or after `from`; `None` if it is not there.
    pub fn on_trajectory(&self, tip: &str, from: usize) -> Option<usize> {
        self.trajectory[from..].iter().position(|t| t == tip).map(|i| i + from)
    }

    /// A block's raw bytes (hex): the chain's, or F01's F or C, which a node stores on a side
    /// branch.
    pub fn block_hex(&self, hash: &str) -> Option<&str> {
        self.blocks.get(hash).map(String::as_str).or_else(|| {
            let f = self.f01.as_ref()?;
            [(&f.f_hash, &f.f_hex), (&f.c_hash, &f.c_hex)]
                .into_iter()
                .find(|(h, _)| h.as_str() == hash)
                .map(|(_, hex)| hex.as_str())
        })
    }
}

pub(super) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}
