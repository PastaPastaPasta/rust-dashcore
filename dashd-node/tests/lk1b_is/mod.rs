//! LK1b-s S1e's fixtures (`tests/data/lk1b-is/{main,rot}.json`, schema `lk1b-is/1`, written by
//! `contrib/node/lk1b-captures`; the README there defines every field) and the per-op comparison of
//! a Rust run against them (PLAN-LK1b §6 S1e "Comparison per op").
//!
//! One fixture step is one op Core's recorder took, with the O8 lines it wrote while the op ran
//! and, on restart steps, the raw IS DB rows `isdump` read from a stopped copy. The comparison
//! takes the same op's records and rows from the Rust side and reports every difference:
//! - the outcome of each `C`/`R`/`D` attempt: kind, identity, `res`, and an `R`'s `cls` and `why`;
//! - each attempt's `is_conflict` queries, in order;
//! - the IS keys (`is_rm`) of every attempt's `fx`, where both sides wrote it; the non-IS keys
//!   (`mn_changed`, `snap`, `qc_unmine`, `ban_reset`) are left out on both sides (their parity is
//!   O2b-5's), so the Rust side may write IS-only `fx`; on the attempts a fixture marks as
//!   conflicts both sides must write `fx`;
//! - at checkpoints, the `is_locks` digest (O2b §2.6) and every raw row.
//!
//! The step after (g)'s KILL that delivers the killed block again is kill-dependent (MN0 Q11):
//! Core's recorder lost the block and connects it again, a Rust node that kept it may treat it as
//! a duplicate. There the Rust side may write Core's `C` (with its `A`) or no attempt at all, and
//! instead of the step's records the full IS state after the step is compared: the rows against
//! the KILL step's checkpoint and the tip against the block.

#[cfg(feature = "test-support")]
pub mod node;
#[cfg(feature = "test-support")]
pub mod store;

use std::collections::HashSet;
use std::path::PathBuf;

use dash_statedigest::{Effect, Hash256, Ident, Kind, Origin, Query, Record, Value, parse_line};
use serde_json::Value as Json;

pub const SCHEMA: &str = "lk1b-is/1";

/// The scenario names a step may carry (README "Fixture schema").
pub const SCENARIOS: &[&str] = &["setup", "a", "b", "c", "c'", "d", "e", "f", "g", "h", "rot"];

/// The scenarios each capture must hold.
pub fn expected_scenarios(run: &str) -> &'static [&'static str] {
    match run {
        "main" => &["a", "b", "c", "c'", "d", "e", "f", "g", "h"],
        "rot" => &["rot"],
        _ => &[],
    }
}

pub fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/lk1b-is")
}

pub fn sample_text() -> String {
    std::fs::read_to_string(data_dir().join("sample.json")).expect("sample fixture")
}

pub fn sample() -> Fixture {
    load(&sample_text()).expect("sample loads")
}

/// A capture fixture (`main` or `rot`), which `contrib/node/lk1b-captures/run.sh` writes.
pub fn capture(run: &str) -> Fixture {
    let path = data_dir().join(format!("{run}.json"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!("{}: {e} (run contrib/node/lk1b-captures/run.sh)", path.display())
    });
    load(&text).unwrap_or_else(|e| panic!("{run}: {e}"))
}

/// The `dash.conf` DashTestFramework gives every node of `run`, the recorder's included, so not in
/// `provenance.recorder_args`: `append_dip3_config`'s chain lines under `write_config`'s section
/// (`test/functional/test_framework/test_framework.py` v24.0.0:1530-1535, `util.py` :414-420). V20
/// and mn_rr activate at `set_dash_test_params`' 100 (:1543-1544), or for the rot run at
/// `V19_HEIGHT`, 2000 (`contrib/node/lq3a-captures/capture_lq3_rotation.py:28, 41`,
/// `delay_v20_and_mn_rr`, :1567-1569).
pub fn framework_conf(run: &str) -> String {
    let v20 = if run == "rot" {
        2000
    } else {
        100
    };
    format!(
        "regtest=1\n[regtest]\ndip3params=2:2\ntestactivationheight=v20@{v20}\n\
         testactivationheight=mn_rr@{v20}\n"
    )
}

/// A data directory for the Rust side of `run`, holding [`framework_conf`] as its `dash.conf`.
pub fn node_datadir(dir: &std::path::Path, run: &str) {
    std::fs::write(dir.join("dash.conf"), framework_conf(run)).expect("dash.conf");
}

/// The command line the Rust side of `run` starts with on a [`node_datadir`]: regtest, node0's
/// spork key (`test_framework.py` :1516-1519, which the recorder's `-sporkkey` repeats), then the
/// recorder's arguments that shape its chain (README "Recorder"); its RPC, logging, indexes and
/// connections are left out.
pub fn node_args(run: &str) -> Vec<String> {
    let path = data_dir().join(format!("{run}.json"));
    let raw: Json = serde_json::from_str(&std::fs::read_to_string(path).expect("the fixture"))
        .expect("the fixture's JSON");
    let recorder = raw["provenance"]["recorder_args"].as_array().expect("recorder_args");
    let chain = recorder.iter().map(|a| a.as_str().expect("an argument")).filter(|a| {
        ["-testactivationheight=", "-vbparams=", "-llmqtest"].iter().any(|p| a.starts_with(p))
    });
    [
        "-regtest",
        "-printtoconsole=0",
        "-sporkkey=cP4EKFyJsHT39LDqgdcB43Y3YXjNyjb5Fuas1GQSeAtjnZWmZEQK",
    ]
    .into_iter()
    .chain(chain)
    .map(str::to_owned)
    .collect()
}

/// The IS DB tables by their serialized key prefix (Core `instantsend/db.cpp:12-18` at v24.0.0).
pub const TABLES: &[(&str, &[u8])] = &[
    ("is_i", b"\x04is_i"),
    ("is_tx", b"\x05is_tx"),
    ("is_in", b"\x05is_in"),
    ("is_m", b"\x04is_m"),
    ("is_a1", b"\x05is_a1"),
    ("is_a2", b"\x05is_a2"),
    ("is_v", b"\x04is_v"),
];

/// The tables O2b §2.6's `is_locks` covers: the locks, their mined heights and the archive.
pub const IS_LOCKS_TABLES: &[&str] = &["is_i", "is_m", "is_a1", "is_a2"];

pub fn table(key: &[u8]) -> Option<&'static str> {
    TABLES.iter().find(|(_, p)| key.starts_with(p)).map(|(name, _)| *name)
}

/// `is_locks`: SHA-256 over the sorted keys of [`IS_LOCKS_TABLES`], each as a u32 LE length and
/// the key bytes. The capture computes the same in Python (`capture_lk1b.is_locks_digest`).
pub fn is_locks_digest<'a>(keys: impl IntoIterator<Item = &'a [u8]>) -> [u8; 32] {
    let mut keys: Vec<&[u8]> = keys
        .into_iter()
        .filter(|k| table(k).is_some_and(|t| IS_LOCKS_TABLES.contains(&t)))
        .collect();
    keys.sort();
    let mut h = dash_sha256::Sha256::new();
    for k in keys {
        h.update(&u32::try_from(k.len()).expect("short key").to_le_bytes());
        h.update(k);
    }
    h.finalize()
}

/// How a restart ends the process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestartKind {
    Clean,
    Kill,
}

/// One op, as the Rust side receives it. `core_tx` (the transaction Core's recorder needed in its
/// mempool to store a lock) is Core-only and not kept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    Block {
        hash: Hash256,
        bytes: Vec<u8>,
        core_reply: Option<String>,
    },
    ClRecord {
        height: i32,
        hash: Hash256,
        sig: [u8; 96],
    },
    IsLock {
        bytes: Vec<u8>,
        inv_hash: Hash256,
        txid: Hash256,
        mined_height: Option<i32>,
    },
    Spork {
        id: u32,
        value: i64,
    },
    Restart {
        kind: RestartKind,
        args: Vec<String>,
    },
}

impl Op {
    pub fn tag(&self) -> &'static str {
        match self {
            Op::Block {
                ..
            } => "BLOCK",
            Op::ClRecord {
                ..
            } => "CL_RECORD",
            Op::IsLock {
                ..
            } => "ISLOCK",
            Op::Spork {
                ..
            } => "SPORK",
            Op::Restart {
                ..
            } => "RESTART",
        }
    }
}

/// Raw rows of one IS DB, in key order.
pub type Rows = Vec<(Vec<u8>, Vec<u8>)>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    pub rows: Rows,
    pub is_locks: [u8; 32],
}

#[derive(Clone, Debug)]
pub struct Step {
    pub step: usize,
    pub scenario: String,
    pub label: String,
    /// The recorder's clock at its last start (README "Fixture schema"); see [`clock`].
    pub mocktime: i64,
    pub op: Op,
    /// Every record Core wrote during the op: the digest's lines, then the decisions'.
    pub records: Vec<Record>,
    /// Attempts (by block hash) that must carry `fx` on both sides.
    pub conflict: Vec<Hash256>,
    pub checkpoint: Option<Checkpoint>,
    /// On the kill-dependent step (the first `BLOCK` after a KILL that delivers a block connected
    /// before the KILL again): the KILL step's checkpoint, which the IS state after this step must
    /// equal (MN0 Q11). The loader derives it.
    pub kill_dependent: Option<Checkpoint>,
    /// On the KILL step: the records Core wrote at the kill-dependent step, as startup's import
    /// would write them. A Rust node that kept the killed block (Core's SIGKILL lost it, its
    /// block index flushed lazily) connects it as it starts, at the KILL step, which may then
    /// write these records after Core's own; the re-delivery is then its duplicate (MN0 Q11).
    /// Startup's `ActivateBestChain` runs inside `CImportingNow`, so `IsInitialBlockDownload()`
    /// holds and `ConnectBlock` skips the InstantSend loop: the `is_conflict` queries are left
    /// out. The loader derives it.
    // Core: node/blockstorage.cpp:898-963, validation.cpp:1990-1992, :2812 at v24.0.0
    pub kill_replay: Option<Vec<Record>>,
}

#[derive(Clone, Debug)]
pub struct Fixture {
    pub run: String,
    pub steps: Vec<Step>,
}

fn field<'a>(obj: &'a Json, key: &str, at: &str) -> Result<&'a Json, String> {
    obj.get(key).ok_or_else(|| format!("{at}: no `{key}`"))
}

fn string<'a>(obj: &'a Json, key: &str, at: &str) -> Result<&'a str, String> {
    field(obj, key, at)?.as_str().ok_or_else(|| format!("{at}: `{key}` is not a string"))
}

fn int(obj: &Json, key: &str, at: &str) -> Result<i64, String> {
    field(obj, key, at)?.as_i64().ok_or_else(|| format!("{at}: `{key}` is not an integer"))
}

fn hex(text: &str, at: &str) -> Result<Vec<u8>, String> {
    dash_statedigest::decode_hex(text.as_bytes()).ok_or_else(|| format!("{at}: bad hex"))
}

fn hash(obj: &Json, key: &str, at: &str) -> Result<Hash256, String> {
    Hash256::from_hex(string(obj, key, at)?.as_bytes())
        .ok_or_else(|| format!("{at}: `{key}` is not a 64-digit hash"))
}

fn hash_field(obj: &Json, key: &str, at: &str) -> Result<Hash256, String> {
    hash(obj, key, at)
}

fn height(obj: &Json, key: &str, at: &str) -> Result<i32, String> {
    i32::try_from(int(obj, key, at)?).map_err(|_| format!("{at}: `{key}` out of range"))
}

fn lines(obj: &Json, key: &str, at: &str) -> Result<Vec<Record>, String> {
    let arr = field(obj, key, at)?.as_array().ok_or_else(|| format!("{at}: `{key}` not a list"))?;
    arr.iter()
        .enumerate()
        .map(|(i, line)| {
            let text = line.as_str().ok_or_else(|| format!("{at}: {key}[{i}] not a string"))?;
            let mut bytes = text.as_bytes().to_vec();
            bytes.push(b'\n');
            parse_line(&bytes).map_err(|e| format!("{at}: {key}[{i}]: {e:?}"))
        })
        .collect()
}

fn sha256d(bytes: &[u8]) -> Hash256 {
    let mut sha = dash_sha256::Sha256d::new();
    sha.update(bytes);
    Hash256::from_wire_bytes(sha.finalize())
}

/// The txid of an `isdlock` payload: version, compact-size input count, the inputs (36 bytes
/// each), then the txid, the cycle hash and the signature.
fn isdlock_txid(bytes: &[u8]) -> Option<Hash256> {
    let (count, skip) = match *bytes.get(1)? {
        n @ 0..0xfd => (usize::from(n), 2),
        0xfd => (usize::from(u16::from_le_bytes(bytes.get(2..4)?.try_into().ok()?)), 4),
        _ => return None,
    };
    let start = skip + count * 36;
    if bytes.len() != start + 32 + 32 + 96 {
        return None;
    }
    Some(Hash256::from_wire_bytes(bytes[start..start + 32].try_into().ok()?))
}

fn op(obj: &Json, at: &str) -> Result<Op, String> {
    let at = &format!("{at} op");
    Ok(match string(obj, "op", at)? {
        "BLOCK" => {
            let bytes = hex(string(obj, "bytes", at)?, at)?;
            if bytes.len() < 81 {
                return Err(format!("{at}: block shorter than a header"));
            }
            let hash = hash(obj, "hash", at)?;
            let header = dash_consensus::decode_header(&mut &bytes[..80])
                .map_err(|e| format!("{at}: header: {e:?}"))?;
            if Hash256::from_wire_bytes(*header.value().hash().as_bytes()) != hash {
                return Err(format!("{at}: hash is not the X11 of the header"));
            }
            Op::Block {
                hash,
                bytes,
                core_reply: field(obj, "core_reply", at)?.as_str().map(str::to_owned),
            }
        }
        "CL_RECORD" => {
            let sig: [u8; 96] = hex(string(obj, "sig", at)?, at)?
                .try_into()
                .map_err(|_| format!("{at}: sig is not 96 bytes"))?;
            let height = height(obj, "height", at)?;
            let hash = hash(obj, "hash", at)?;
            // The `clsig` payload: the height (i32 LE), the block hash and the signature. The
            // fixture keeps its fields, so the payload is theirs exactly when `inv_hash` matches.
            let mut payload = height.to_le_bytes().to_vec();
            payload.extend(hash.to_wire_bytes());
            payload.extend(sig);
            if sha256d(&payload) != hash_field(obj, "inv_hash", at)? {
                return Err(format!(
                    "{at}: inv_hash is not sha256d of the clsig payload (height, hash, sig)"
                ));
            }
            Op::ClRecord {
                height,
                hash,
                sig,
            }
        }
        "ISLOCK" => {
            let bytes = hex(string(obj, "bytes", at)?, at)?;
            let inv_hash = hash(obj, "inv_hash", at)?;
            if sha256d(&bytes) != inv_hash {
                return Err(format!("{at}: inv_hash is not sha256d(bytes)"));
            }
            let txid = hash(obj, "txid", at)?;
            if isdlock_txid(&bytes) != Some(txid) {
                return Err(format!("{at}: txid is not the payload's"));
            }
            let mined = field(obj, "mined_height", at)?;
            Op::IsLock {
                bytes,
                inv_hash,
                txid,
                mined_height: if mined.is_null() {
                    None
                } else {
                    Some(height(obj, "mined_height", at)?)
                },
            }
        }
        "SPORK" => Op::Spork {
            id: u32::try_from(int(obj, "id", at)?).map_err(|_| format!("{at}: bad spork id"))?,
            value: int(obj, "value", at)?,
        },
        "RESTART" => Op::Restart {
            kind: match string(obj, "kind", at)? {
                "CLEAN" => RestartKind::Clean,
                "KILL" => RestartKind::Kill,
                k => return Err(format!("{at}: unknown restart kind {k}")),
            },
            args: field(obj, "args", at)?
                .as_array()
                .ok_or_else(|| format!("{at}: args not a list"))?
                .iter()
                .map(|a| a.as_str().map(str::to_owned).ok_or_else(|| format!("{at}: bad arg")))
                .collect::<Result<_, _>>()?,
        },
        other => return Err(format!("{at}: unknown op {other}")),
    })
}

fn checkpoint(obj: &Json, at: &str) -> Result<Option<Checkpoint>, String> {
    let ckpt = field(obj, "checkpoint", at)?;
    if ckpt.is_null() {
        return Ok(None);
    }
    let at = &format!("{at} checkpoint");
    let mut rows = Vec::new();
    for (i, row) in field(ckpt, "rows", at)?.as_array().ok_or("rows not a list")?.iter().enumerate()
    {
        let row = row.as_array().filter(|r| r.len() == 3).ok_or(format!("{at}: row {i}"))?;
        let key = hex(row[0].as_str().ok_or(format!("{at}: row {i} key"))?, at)?;
        let value = hex(row[1].as_str().ok_or(format!("{at}: row {i} value"))?, at)?;
        if row[2].as_str() != table(&key) {
            return Err(format!("{at}: row {i} names table {} for its key", row[2]));
        }
        rows.push((key, value));
    }
    if !rows.windows(2).all(|w| w[0].0 < w[1].0) {
        return Err(format!("{at}: rows not in strict key order"));
    }
    let is_locks: [u8; 32] = hex(string(ckpt, "is_locks", at)?, at)?
        .try_into()
        .map_err(|_| format!("{at}: is_locks is not 32 bytes"))?;
    if is_locks_digest(rows.iter().map(|(k, _)| k.as_slice())) != is_locks {
        return Err(format!("{at}: is_locks does not match its rows"));
    }
    Ok(Some(Checkpoint {
        rows,
        is_locks,
    }))
}

/// Parses and checks a fixture: the schema, consecutive step numbers, every op's shape (a block's
/// hash is the X11 of its header, a `clsig`'s and an isdlock's inv hash their payload's sha256d),
/// every O8 line canonical, each digest line a `H`/`C`/`R`/`D` and each decisions line a `H`/`A`,
/// every marked conflict attempt present among the step's records, the scenario names (and every
/// scenario its run must hold), checkpoints on exactly the RESTART and KILL steps and the last
/// step, and every checkpoint's `is_locks` equal to its rows' digest.
pub fn load(text: &str) -> Result<Fixture, String> {
    let root: Json = serde_json::from_str(text).map_err(|e| format!("fixture JSON: {e}"))?;
    if string(&root, "schema", "fixture")? != SCHEMA {
        return Err(format!("fixture: schema is not {SCHEMA}"));
    }
    let run = string(&root, "run", "fixture")?.to_owned();
    let mut steps = Vec::new();
    // Scenario (d)'s ChainLocked invalid block and its reject outcome, once its CL_RECORD ran.
    let mut retried: Option<AttemptView> = None;
    // Every block a `C` connected so far, and the last KILL's checkpoint with the blocks connected
    // before it, until the first BLOCK after it.
    let mut connected: HashSet<Hash256> = HashSet::new();
    let mut killed: Option<(Checkpoint, HashSet<Hash256>)> = None;
    let mut kill_dependent_steps = 0usize;
    let step_list =
        field(&root, "steps", "fixture")?.as_array().ok_or("fixture: steps not a list")?;
    let step_count = step_list.len();
    for (i, s) in step_list.iter().enumerate() {
        let at = &format!("step {}", i + 1);
        let number = usize::try_from(int(s, "step", at)?).map_err(|_| format!("{at}: number"))?;
        if number != i + 1 {
            return Err(format!("{at}: numbered {number}"));
        }
        let digest = lines(s, "digest", at)?;
        if let Some(r) =
            digest.iter().find(|r| !matches!(r, Record::Header(_) | Record::Outcome(_)))
        {
            return Err(format!("{at}: a {:?} record among the digest lines", r.kind()));
        }
        let decisions = lines(s, "decisions", at)?;
        if let Some(r) =
            decisions.iter().find(|r| !matches!(r, Record::Header(_) | Record::Attempt(_)))
        {
            return Err(format!("{at}: a {:?} record among the decision lines", r.kind()));
        }
        let mut records = digest;
        records.extend(decisions);
        let conflict = match field(s, "conflict", at)? {
            Json::Null => Vec::new(),
            Json::Array(hashes) => hashes
                .iter()
                .map(|h| {
                    h.as_str()
                        .and_then(|h| Hash256::from_hex(h.as_bytes()))
                        .ok_or(format!("{at}: bad conflict hash"))
                })
                .collect::<Result<_, _>>()?,
            _ => return Err(format!("{at}: conflict is neither null nor a list")),
        };
        for b in &conflict {
            if !attempts(&records).iter().any(|a| &a.id.b == b && a.kind != Kind::Disconnect) {
                return Err(format!("{at}: conflict attempt {} has no record", b.to_hex()));
            }
        }
        let scenario = string(s, "scenario", at)?.to_owned();
        if !SCENARIOS.contains(&scenario.as_str()) {
            return Err(format!("{at}: unknown scenario {scenario:?}"));
        }
        let op = op(field(s, "op", at)?, at)?;
        if scenario != "d" {
            retried = None;
        }
        if let (
            true,
            Op::ClRecord {
                hash,
                ..
            },
        ) = (scenario == "d", &op)
            && conflict.contains(hash)
        {
            retried = attempts(&records).into_iter().rfind(|a| &a.id.b == hash);
        }
        // The ChainLock timer's retries (README "periodic"): canonical, not compared, and only
        // ever more rejects of (d)'s ChainLocked block (MN0 ruling Q7).
        if s.get("periodic").is_some() {
            for r in lines(s, "periodic", at)? {
                let ok = match (&r, &retried) {
                    (Record::Outcome(_), Some(first)) => {
                        attempts(std::slice::from_ref(&r)).first().is_some_and(|a| {
                            a.kind == Kind::Reject
                                && a.id.b == first.id.b
                                && a.outcome == first.outcome
                                && !a.fx.iter().flatten().any(|e| matches!(e, Effect::IsRm { .. }))
                        })
                    }
                    (Record::Attempt(a), Some(first)) => a.id.b == first.id.b,
                    _ => false,
                };
                if !ok || retried.as_ref().is_some_and(|f| f.kind != Kind::Reject) {
                    return Err(format!(
                        "{at}: periodic record is not a retry of (d)'s ChainLocked block"
                    ));
                }
            }
        }
        // A restart ends the retries: the ChainLock is held in memory only.
        if matches!(op, Op::Restart { .. }) {
            retried = None;
        }
        let checkpoint = checkpoint(s, at)?;
        // Rows are dumped at each RESTART and KILL and after the last op, nowhere else.
        let restart = matches!(op, Op::Restart { .. });
        if checkpoint.is_some() && !restart && i + 1 != step_count {
            return Err(format!("{at}: a checkpoint on a {} step", op.tag()));
        }
        if checkpoint.is_none() && restart {
            return Err(format!("{at}: a RESTART without a checkpoint"));
        }
        let mut kill_dependent = None;
        if let Op::Block {
            hash,
            ..
        } = &op
            && let Some((ckpt, before)) = killed.take()
            && before.contains(hash)
        {
            kill_dependent = Some(ckpt);
            kill_dependent_steps += 1;
        }
        if let (
            Op::Restart {
                kind: RestartKind::Kill,
                ..
            },
            Some(ckpt),
        ) = (&op, &checkpoint)
        {
            killed = Some((ckpt.clone(), connected.clone()));
        }
        connected.extend(
            attempts(&records).into_iter().filter(|a| a.kind == Kind::Connect).map(|a| a.id.b),
        );
        steps.push(Step {
            step: number,
            scenario,
            label: string(s, "label", at)?.to_owned(),
            mocktime: int(s, "mocktime", at)?,
            op,
            records,
            conflict,
            checkpoint,
            kill_dependent,
            kill_replay: None,
        });
    }
    // The KILL step before each kill-dependent step holds that step's records.
    for i in 0..steps.len() {
        if steps[i].kill_dependent.is_some() {
            let kill = steps[..i]
                .iter()
                .rposition(|s| {
                    matches!(
                        s.op,
                        Op::Restart {
                            kind: RestartKind::Kill,
                            ..
                        }
                    )
                })
                .ok_or_else(|| format!("step {}: a kill-dependent step without a KILL", i + 1))?;
            let records = &steps[i].records;
            if attempts(records).iter().any(|a| a.fx.iter().any(|fx| !is_effects(fx).is_empty())) {
                return Err(format!("step {}: a kill-dependent step with IS effects", i + 1));
            }
            // Only a query that found no conflict may go: one that found one decided the block.
            if is_conflict_queries(records).iter().flat_map(|(_, q)| q).any(|q| {
                matches!(
                    q,
                    Query::IsConflict {
                        conflict: Some(_),
                        ..
                    }
                )
            }) {
                return Err(format!("step {}: a kill-dependent step with an IS conflict", i + 1));
            }
            let mut replay = records.clone();
            for r in &mut replay {
                if let Record::Attempt(a) = r {
                    a.q.retain(|q| !matches!(q, Query::IsConflict { .. }));
                }
            }
            if steps[kill].kill_replay.replace(replay).is_some() {
                return Err(format!("step {}: a second kill-dependent step after one KILL", i + 1));
            }
        }
    }
    for scenario in expected_scenarios(&run) {
        if !steps.iter().any(|s: &Step| s.scenario == *scenario) {
            return Err(format!("fixture {run}: no step of scenario ({scenario})"));
        }
    }
    // MN0 Q11: (g)'s re-delivery is main's only kill-dependent step; no other run has a KILL.
    let want = usize::from(run == "main");
    if kill_dependent_steps != want {
        return Err(format!(
            "fixture {run}: {kill_dependent_steps} kill-dependent steps, want {want}"
        ));
    }
    Ok(Fixture {
        run,
        steps,
    })
}

/// One `C`, `R` or `D` attempt, reduced to what S1e compares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttemptView {
    pub kind: Kind,
    pub id: Ident,
    /// `res`, `cls` and `why` (`R`), `res` (`D`), nothing (`C`).
    pub outcome: Vec<(&'static str, Value)>,
    pub fx: Option<Vec<Effect>>,
}

/// The attempts of a record list, in record order. VerifyDB's (`org` `verifydb`) are left out,
/// as node-l1's comparator leaves them out (`node-l1/src/chain/compare.rs` `split`).
pub fn attempts(records: &[Record]) -> Vec<AttemptView> {
    records
        .iter()
        .filter_map(|r| match r {
            Record::Outcome(o) if o.id.org != Origin::Verifydb => {
                let keys: &[&'static str] = match o.kind() {
                    Kind::Reject => &["res", "cls", "why"],
                    Kind::Disconnect => &["res"],
                    _ => &[],
                };
                Some(AttemptView {
                    kind: o.kind(),
                    id: o.id.clone(),
                    outcome: keys
                        .iter()
                        .map(|k| (*k, o.s().get(k).cloned().unwrap_or(Value::Null)))
                        .collect(),
                    fx: o.o().get("fx").map(|v| Effect::list_from_value(v).expect("parsed fx")),
                })
            }
            _ => None,
        })
        .collect()
}

/// Each `A` record's identity and its `is_conflict` queries, in record order, VerifyDB's left out.
pub fn is_conflict_queries(records: &[Record]) -> Vec<(Ident, Vec<Query>)> {
    records
        .iter()
        .filter_map(|r| match r {
            Record::Attempt(a) if a.id.org != Origin::Verifydb => Some((
                a.id.clone(),
                a.q.iter().filter(|q| matches!(q, Query::IsConflict { .. })).cloned().collect(),
            )),
            _ => None,
        })
        .collect()
}

/// What the Rust side produced for one op.
#[derive(Clone, Debug, Default)]
pub struct Observed {
    pub records: Vec<Record>,
    /// The IS DB's rows after the op; required on checkpoint and kill-dependent steps.
    pub rows: Option<Rows>,
    /// The active chain's tip after the op; required on the kill-dependent step and compared only
    /// there.
    pub tip: Option<Hash256>,
}

/// One difference between Core's step and the Rust side's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mismatch {
    pub step: usize,
    pub what: String,
}

/// The IS keys of an `fx` list (`is_rm`), in order: the only keys S1e compares on any `C`, `R` or
/// `D`; the rest are O2b-5's (MN0 Q1, Q10).
pub fn is_effects(fx: &[Effect]) -> Vec<Effect> {
    fx.iter().filter(|e| matches!(e, Effect::IsRm { .. })).cloned().collect()
}

/// Compares one op (PLAN-LK1b §6 S1e "Comparison per op"). On the kill-dependent step (MN0 Q11)
/// the Rust side may write Core's records or no attempt at all, and its rows and tip are compared
/// with the KILL step's checkpoint and the op's block. On the KILL step before it the Rust side
/// may write Core's records alone, or followed by the kill-dependent step's as startup's import
/// ([`Step::kill_replay`]), each compared in full.
pub fn compare_step(step: &Step, rust: &Observed) -> Vec<Mismatch> {
    let mut out = Vec::new();
    let duplicate = step.kill_dependent.is_some()
        && attempts(&rust.records).is_empty()
        && is_conflict_queries(&rust.records).is_empty();
    if !duplicate {
        let strict = compare_records(step, rust);
        match &step.kill_replay {
            Some(replay) if !strict.is_empty() => {
                let with = Step {
                    records: step.records.iter().chain(replay).cloned().collect(),
                    ..step.clone()
                };
                let with = compare_records(&with, rust);
                if !with.is_empty() {
                    out.extend(strict);
                    out.extend(with.into_iter().map(|m| format!("with the re-delivery: {m}")));
                }
            }
            _ => out.extend(strict),
        }
    }
    if let Some(base) = &step.kill_dependent {
        out.extend(compare_rows(base, rust.rows.as_ref(), "kill-dependent state"));
        let Op::Block {
            hash,
            ..
        } = &step.op
        else {
            unreachable!("the loader marks BLOCK steps only")
        };
        match rust.tip {
            None => out.push("kill-dependent state: the Rust side gave no tip".into()),
            Some(tip) if tip != *hash => out.push(format!(
                "kill-dependent state: tip {}, want {}",
                tip.to_hex(),
                hash.to_hex()
            )),
            Some(_) => {}
        }
    } else if let Some(ckpt) = &step.checkpoint {
        out.extend(compare_rows(ckpt, rust.rows.as_ref(), "checkpoint"));
    }
    out.into_iter()
        .map(|what| Mismatch {
            step: step.step,
            what,
        })
        .collect()
}

/// The rows the Rust side gave against a checkpoint: `is_locks` and every row.
fn compare_rows(ckpt: &Checkpoint, rows: Option<&Rows>, what: &str) -> Vec<String> {
    let Some(rows) = rows else {
        return vec![format!("{what}: the Rust side gave no rows")];
    };
    let mut out = Vec::new();
    if is_locks_digest(rows.iter().map(|(k, _)| k.as_slice())) != ckpt.is_locks {
        out.push(format!("{what}: is_locks differs"));
    }
    let diff = row_diff(&ckpt.rows, rows);
    if !diff.is_empty() {
        out.push(format!("{what} rows: {}", diff.join("; ")));
    }
    out
}

/// The record planes of one op: outcomes, the IS keys of `fx`, the conflict attempts' `fx`, the
/// `is_conflict` queries.
fn compare_records(step: &Step, rust: &Observed) -> Vec<String> {
    let mut out = Vec::new();
    let mut miss = |what: String| out.push(what);
    let core_attempts = attempts(&step.records);
    let rust_attempts = attempts(&rust.records);
    let core_outcomes: Vec<_> = core_attempts.iter().map(|a| (a.kind, &a.id, &a.outcome)).collect();
    let rust_outcomes: Vec<_> = rust_attempts.iter().map(|a| (a.kind, &a.id, &a.outcome)).collect();
    if core_outcomes != rust_outcomes {
        miss(format!("outcomes: core {core_outcomes:?}, rust {rust_outcomes:?}"));
    }
    for (c, r) in core_attempts.iter().zip(&rust_attempts) {
        if let (Some(cf), Some(rf)) = (&c.fx, &r.fx)
            && is_effects(cf) != is_effects(rf)
        {
            miss(format!("fx of {}: core {cf:?}, rust {rf:?}", c.id.b.to_hex()));
        }
    }
    for b in &step.conflict {
        for (side, list) in [("core", &core_attempts), ("rust", &rust_attempts)] {
            let found = list.iter().rfind(|a| &a.id.b == b && a.kind != Kind::Disconnect);
            match found {
                None => {
                    miss(format!("{side}: no attempt record for conflict block {}", b.to_hex()))
                }
                Some(a) if a.fx.is_none() => {
                    miss(format!("{side}: no fx on conflict attempt {}", b.to_hex()))
                }
                Some(_) => {}
            }
        }
    }
    let core_q = is_conflict_queries(&step.records);
    let rust_q = is_conflict_queries(&rust.records);
    if core_q != rust_q {
        miss(format!("is_conflict queries: core {core_q:?}, rust {rust_q:?}"));
    }
    out
}

fn hexs(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Row differences, by key: rows only Core has, rows only Rust has, values that differ.
pub fn row_diff(core: &Rows, rust: &Rows) -> Vec<String> {
    use std::collections::BTreeMap;
    let c: BTreeMap<_, _> = core.iter().map(|(k, v)| (k, v)).collect();
    let r: BTreeMap<_, _> = rust.iter().map(|(k, v)| (k, v)).collect();
    let mut out = Vec::new();
    for (k, v) in &c {
        match r.get(k) {
            None => out.push(format!("core only {}", hexs(k))),
            Some(rv) if rv != v => {
                out.push(format!("{}: core {}, rust {}", hexs(k), hexs(v), hexs(rv)))
            }
            Some(_) => {}
        }
    }
    out.extend(r.keys().filter(|k| !c.contains_key(*k)).map(|k| format!("rust only {}", hexs(k))));
    out
}

/// The Rust side of the differential: a node that takes the fixture's ops one at a time.
///
/// Step 1 of each fixture carries the recorder's start-up records (the genesis `C` and its `A`)
/// under its first op, a SPORK: the first op's records include start-up, so the Rust side must
/// emit its own start-up records in its first [`apply`](IsSide::apply).
pub trait IsSide {
    /// Sets the clock before a step's op, to [`clock`]'s time for the step.
    fn set_time(&mut self, mocktime: i64);
    /// Applies one op and returns the records it wrote and, on restart ops, the IS DB's rows as
    /// the teardown left them. On the kill-dependent step (the block (g)'s KILL lost, delivered
    /// again) it also returns the IS DB's rows and the chain tip after the op ([`Observed::tip`]).
    fn apply(&mut self, op: &Op) -> Result<Observed, String>;
}

/// The time the Rust side's clock is set to before a step's op: the `t` of the step's first `A`
/// record, else the step's `mocktime` (the recorder's clock at its last start rather than when the
/// op ran), and never earlier than the previous step's clock `prev`, as Core's clock never ran
/// backwards.
pub fn clock(prev: Option<i64>, step: &Step) -> i64 {
    let t = step
        .records
        .iter()
        .find_map(|r| match r {
            Record::Attempt(a) => Some(a.t),
            _ => None,
        })
        .unwrap_or(step.mocktime);
    prev.map_or(t, |p| p.max(t))
}

/// Drives a side through every step and collects every mismatch.
pub fn drive(fixture: &Fixture, side: &mut dyn IsSide) -> Result<Vec<Mismatch>, String> {
    let mut out = Vec::new();
    let mut now = None;
    for step in &fixture.steps {
        let t = clock(now, step);
        now = Some(t);
        side.set_time(t);
        let observed = side.apply(&step.op).map_err(|e| format!("step {}: {e}", step.step))?;
        out.extend(compare_step(step, &observed));
    }
    Ok(out)
}
