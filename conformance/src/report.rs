//! Mismatch records, statistics, the NDJSON report writer, checkpoints and the gate.
//!
//! Layout of an output directory:
//! - `meta.json`: the network, source, RPC chunk size or blocks directory ([`Meta`]);
//! - `runs.ndjson`: one line per harness run, with its [`Identity`];
//! - `mismatches.ndjson`: one [`Mismatch`] per line, appended as they are found;
//! - `progress.ndjson`: one [`UnitDone`] per finished work unit (RPC height chunk or blk
//!   file) with its identity and [`Coverage`], used by `--resume`, the gate and the summary;
//! - `invalidated.ndjson`: unit results a later run found no longer valid (reorg, changed
//!   blk file);
//! - `summary.json`: a [`Summary`] rebuilt from the files above at the end of each run.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// Largest transaction (in bytes) whose hex is embedded in a mismatch record as a repro.
pub const MAX_REPRO_TX_LEN: usize = 16 * 1024;

/// Which side of the comparison is (probably) wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Side {
    /// rust-dashcore (`dashcore`) disagrees with the input bytes or the oracle.
    Dashcore,
    /// base-sdk (`dash_primitives`) disagrees with the input bytes or the oracle.
    BaseSdk,
    /// Both libraries disagree with the input bytes or the oracle in the same way.
    Both,
    /// The libraries disagree with each other and nothing independent decides who is right.
    Undecided,
    /// The harness itself could not process the input (I/O, truncated record, RPC error).
    Harness,
    /// The node's `dash_consensus` wrappers disagree with the input bytes or the reference.
    DashConsensus,
}

/// One mismatch, with enough context to reproduce it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mismatch {
    pub net: String,
    /// `rpc` or `blk`.
    pub source: String,
    pub height: Option<u32>,
    /// Block hash in display (reversed) hex, as dashd prints it.
    pub block_hash: Option<String>,
    pub tx_index: Option<usize>,
    pub txid: Option<String>,
    /// Stable check identifier, e.g. `tx.decode` or `payload.field`.
    pub check: String,
    pub side: Side,
    /// Field path for field-level checks, e.g. `ProRegTx.platform_node_id`.
    pub field: Option<String>,
    pub expected: Option<String>,
    pub actual: Option<String>,
    pub detail: String,
    /// Raw transaction hex (only for transactions up to [`MAX_REPRO_TX_LEN`] bytes).
    pub tx_hex: Option<String>,
    /// Work unit and run that produced this record (set by [`Reporter::record_mismatches`]).
    /// The summary only counts records of the latest run of each unit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
}

/// Checks that are informational: recorded, but never a reason to fail the gate. Only blk
/// records dashd also leaves off its active chain (see [`crate::run::Unlinked`]); anything else
/// the harness cannot verify is a failing finding.
pub const INFORMATIONAL_CHECKS: &[&str] = &["blk.unlinked_ahead", "blk.unlinked_stale"];

/// Checks that mean blocks may have gone unchecked (blk bytes skipped, blocks not linked to
/// the chain, a requested range the chain does not reach). A gate with any of them is
/// incomplete, not just failing.
pub const COVERAGE_CHECKS: &[&str] = &["blk.scan", "blk.scan_tail", "blk.orphan", "blk.range"];

impl Mismatch {
    /// Whether this record is informational (see [`INFORMATIONAL_CHECKS`]).
    pub fn is_informational(&self) -> bool {
        INFORMATIONAL_CHECKS.contains(&self.check.as_str())
    }

    /// The key mismatches are grouped by in the summary.
    pub fn key(&self) -> String {
        match &self.field {
            Some(f) => format!("{}:{}:{}", self.check, f, side_name(self.side)),
            None => format!("{}:{}", self.check, side_name(self.side)),
        }
    }
}

fn side_name(side: Side) -> &'static str {
    match side {
        Side::Dashcore => "dashcore",
        Side::BaseSdk => "base-sdk",
        Side::Both => "both",
        Side::Undecided => "undecided",
        Side::Harness => "harness",
        Side::DashConsensus => "dash-consensus",
    }
}

/// Counters accumulated while checking.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Stats {
    pub blocks: u64,
    pub stale_blocks: u64,
    pub bytes: u64,
    pub txs: u64,
    /// Special transactions by `nType` (as Core reads it).
    pub special_txs: BTreeMap<u16, u64>,
    /// Blocks cross-checked against `getblock <hash> 2`.
    pub verbose_blocks: u64,
    /// Blocks whose hash was checked against an independent reference (oracle or chain link).
    pub oracle_hash_checks: u64,
    pub mismatches: u64,
    /// Blocks the `dash_consensus` leg checked.
    #[serde(default)]
    pub consensus_blocks: u64,
    /// Blocks the base-sdk decode judges (header, block) checked (`bsdk-judges` feature).
    #[serde(default)]
    pub bsdk_blocks: u64,
    /// Transactions the base-sdk transaction judge checked.
    #[serde(default)]
    pub bsdk_txs: u64,
    /// Special-transaction payloads (types 1 to 12) judged under both BLS scheme flags.
    #[serde(default)]
    pub bsdk_payloads: u64,
    /// How many of `mismatches` are informational (see [`INFORMATIONAL_CHECKS`]).
    #[serde(default)]
    pub informational: u64,
    /// Mismatches counted but not written because their group hit the line cap.
    #[serde(default)]
    pub suppressed: BTreeMap<String, Suppressed>,
    pub min_height: Option<u32>,
    pub max_height: Option<u32>,
}

impl Stats {
    pub fn merge(&mut self, other: &Stats) {
        self.blocks += other.blocks;
        self.stale_blocks += other.stale_blocks;
        self.bytes += other.bytes;
        self.txs += other.txs;
        for (k, v) in &other.special_txs {
            *self.special_txs.entry(*k).or_default() += v;
        }
        self.verbose_blocks += other.verbose_blocks;
        self.oracle_hash_checks += other.oracle_hash_checks;
        self.mismatches += other.mismatches;
        self.consensus_blocks += other.consensus_blocks;
        self.bsdk_blocks += other.bsdk_blocks;
        self.bsdk_txs += other.bsdk_txs;
        self.bsdk_payloads += other.bsdk_payloads;
        self.informational += other.informational;
        for (k, v) in &other.suppressed {
            self.suppressed.entry(k.clone()).or_default().merge(v);
        }
        self.min_height = min_opt(self.min_height, other.min_height);
        self.max_height = self.max_height.max(other.max_height);
    }

    /// Counts mismatches found outside `check_block` (e.g. by a driver).
    pub fn count(&mut self, mismatches: &[Mismatch]) {
        self.mismatches += mismatches.len() as u64;
        self.informational += mismatches.iter().filter(|m| m.is_informational()).count() as u64;
    }

    pub fn saw_height(&mut self, height: u32) {
        self.min_height = min_opt(self.min_height, Some(height));
        self.max_height = self.max_height.max(Some(height));
    }
}

/// Mismatches of one group that were counted but not written (see [`Reporter::open`]).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Suppressed {
    pub count: u64,
    pub first_height: Option<u32>,
    pub last_height: Option<u32>,
}

impl Suppressed {
    fn add(&mut self, height: Option<u32>) {
        self.count += 1;
        self.first_height = min_opt(self.first_height, height);
        self.last_height = self.last_height.max(height);
    }

    fn merge(&mut self, other: &Suppressed) {
        self.count += other.count;
        self.first_height = min_opt(self.first_height, other.first_height);
        self.last_height = self.last_height.max(other.last_height);
    }
}

fn min_opt(a: Option<u32>, b: Option<u32>) -> Option<u32> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// FNV-1a over 128 bits, for identities and selection signatures (change detection, not
/// security).
#[derive(Debug, Clone)]
pub struct Fnv128(u128);

impl Default for Fnv128 {
    fn default() -> Self {
        Self(0x6c62_272e_07bb_0142_62b8_2175_6295_c58d)
    }
}

impl Fnv128 {
    /// Adds one length-delimited field.
    pub fn eat(&mut self, bytes: &[u8]) -> &mut Self {
        const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;
        for b in bytes.iter().chain(&(bytes.len() as u64).to_le_bytes()) {
            self.0 = (self.0 ^ u128::from(*b)).wrapping_mul(PRIME);
        }
        self
    }

    /// Adds a large blob, eight bytes at a time (fast enough for an executable in a debug
    /// build).
    pub fn eat_blob(&mut self, bytes: &[u8]) -> &mut Self {
        const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;
        for chunk in bytes.chunks(8) {
            let mut word = [0u8; 8];
            word[..chunk.len()].copy_from_slice(chunk);
            self.0 = (self.0 ^ u128::from(u64::from_le_bytes(word))).wrapping_mul(PRIME);
        }
        self.eat(&[])
    }

    pub fn hex(&self) -> String {
        format!("{:032x}", self.0)
    }
}

/// Digest of the running executable, computed once.
fn executable_digest() -> &'static str {
    static DIGEST: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    DIGEST.get_or_init(|| {
        let exe = std::env::current_exe().expect("path of the running executable");
        let bytes =
            std::fs::read(&exe).unwrap_or_else(|e| panic!("reading {}: {e}", exe.display()));
        Fnv128::default().eat_blob(&bytes).hex()
    })
}

/// What produced a checkpoint. Checkpoints of another identity are never reused or counted,
/// so changing a library, the harness, the oracle or the policy re-checks everything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    /// The code: the source identity computed by `build.rs` (the harness, `dashcore` and
    /// base-sdk) and a digest of the running executable, which also covers what a build script
    /// cannot see (Cargo feature unification, compiler version and flags).
    pub code: String,
    /// The reference the verdicts come from (RPC mode: the oracle's version); empty in blk
    /// mode, whose references come from the blk files.
    #[serde(default)]
    pub oracle: String,
    /// The comparison policy: which checks ran.
    pub policy: String,
    /// base-sdk revisions in the build, for reports.
    pub base_sdk: String,
}

impl Identity {
    /// The identity of this build with the given comparison policy.
    pub fn of_build(policy: impl Into<String>) -> Self {
        Self {
            code: format!("{}.{}", env!("CONFORMANCE_CODE_ID"), executable_digest()),
            oracle: String::new(),
            policy: policy.into(),
            base_sdk: env!("CONFORMANCE_BASE_SDK").to_string(),
        }
    }

    /// The same identity, judged against `oracle`.
    pub fn with_oracle(self, oracle: impl Into<String>) -> Self {
        Self {
            oracle: oracle.into(),
            ..self
        }
    }

    /// The compact form stored with every checkpoint.
    pub fn id(&self) -> String {
        let mut h = Fnv128::default();
        h.eat(self.code.as_bytes()).eat(self.oracle.as_bytes()).eat(self.policy.as_bytes());
        h.hex()
    }

    /// The compact form of the policy alone. A result is only considered re-checked by a unit
    /// of the same policy: a weaker policy does not clear a stronger one's findings.
    pub fn policy_id(&self) -> String {
        Fnv128::default().eat(self.policy.as_bytes()).hex()
    }
}

/// Format of the checkpoints in `progress.ndjson`. A checkpoint of another format is never
/// reused or counted (it is `unverified` until re-checked): format 1 lacked
/// [`Coverage::Heights::prev_hash`], without which a run cannot show that its RPC units form
/// one chain.
pub const CHECKPOINT_FORMAT: u32 = 2;

/// The blocks a finished unit checked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum Coverage {
    /// Heights `lo..=hi` of the oracle's chain, linked to each other, whose block at `hi` had
    /// hash `end_hash` (display hex). The end hash pins the whole range: a reorg below `hi`
    /// changes it.
    Heights {
        lo: u32,
        hi: u32,
        /// The parent of the block at `lo` (display hex): what the unit links to below. `None`
        /// for genesis, and in a [`Requirement`], which only names the end. A run checks that
        /// its units link to each other through it (see `run::run_rpc`).
        #[serde(default)]
        prev_hash: Option<String>,
        end_hash: String,
    },
    /// The selected blocks of one blk file, identified by a signature over their identities,
    /// reference hashes, chain positions and the file's generation.
    Selection {
        sig: String,
    },
    /// The blk index (every file); each run's index replaces the previous one.
    Index,
}

impl Coverage {
    /// Whether everything `other` checked is also checked by `self` (on the same chain).
    /// `prev_hash` is not compared: the end hash commits to every block below it.
    pub fn covers(&self, other: &Coverage) -> bool {
        match (self, other) {
            (
                Coverage::Heights {
                    lo,
                    hi,
                    end_hash,
                    ..
                },
                Coverage::Heights {
                    lo: lo2,
                    hi: hi2,
                    end_hash: end2,
                    ..
                },
            ) => lo <= lo2 && hi2 <= hi && (hi != hi2 || end_hash == end2),
            (
                Coverage::Selection {
                    sig,
                },
                Coverage::Selection {
                    sig: sig2,
                },
            ) => sig == sig2,
            (Coverage::Index, Coverage::Index) => true,
            _ => false,
        }
    }

    /// Whether `self` re-checks the blocks of `old`, a result recorded under another identity
    /// or chain state (so that `old` need not be reported as unverified).
    fn rechecks(&self, old: Option<&Coverage>) -> bool {
        match (self, old) {
            (
                Coverage::Heights {
                    lo,
                    hi,
                    ..
                },
                Some(Coverage::Heights {
                    lo: lo2,
                    hi: hi2,
                    ..
                }),
            ) => lo <= lo2 && hi2 <= hi,
            // A file is re-checked in its current state; an older selection of it is replaced.
            (
                Coverage::Selection {
                    ..
                },
                Some(Coverage::Selection {
                    ..
                }),
            ) => true,
            (Coverage::Index, Some(Coverage::Index)) => true,
            // Records that predate coverage tracking are replaced by any current unit.
            (_, None) => true,
            _ => false,
        }
    }
}

/// A finished unit of work, recorded for `--resume` and the summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnitDone {
    /// The unit's group: `rpc:<chunk start>`, `blk:<file number>` or `blk:index`. Records
    /// written before identities were tracked have `<group>@<details>` here.
    pub unit: String,
    /// Identifies the harness run that checked the unit (see [`Mismatch::run`]).
    #[serde(default)]
    pub run: String,
    /// [`Identity::id`] of the run; empty for records that predate identities.
    #[serde(default)]
    pub identity: String,
    /// [`Identity::policy_id`] of the run.
    #[serde(default)]
    pub policy: String,
    #[serde(default)]
    pub coverage: Option<Coverage>,
    /// [`CHECKPOINT_FORMAT`] of the record; 0 for records that predate it.
    #[serde(default)]
    pub format: u32,
    pub stats: Stats,
    pub seconds: f64,
}

impl UnitDone {
    pub fn group(&self) -> &str {
        let group = self.unit.split('@').next().unwrap_or(&self.unit);
        // The oldest records were `rpc:<start>-<end>`.
        match (&self.coverage, group.strip_prefix("rpc:")) {
            (None, Some(range)) => {
                range.split_once('-').map_or(group, |(start, _)| &group[..4 + start.len()])
            }
            _ => group,
        }
    }
}

/// A unit result that no longer holds: its blocks are no longer the oracle's chain (reorg), or
/// the blk file changed. Written to `invalidated.ndjson`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Invalidated {
    pub unit: String,
    pub run: String,
    pub by_run: String,
    pub reason: String,
}

/// One harness run, appended to `runs.ndjson` when the output directory is opened.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RunRecord {
    run: String,
    identity: String,
    #[serde(default)]
    policy_id: String,
    code: String,
    #[serde(default)]
    oracle: String,
    policy: String,
    base_sdk: String,
}

/// Something a run must have covered for its gate to pass: `coverage` of `unit`, under the
/// run's identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requirement {
    pub unit: String,
    pub coverage: Coverage,
}

/// Mismatch group in the summary.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MismatchGroup {
    pub count: u64,
    pub first_height: Option<u32>,
    pub last_height: Option<u32>,
    pub examples: Vec<Mismatch>,
}

impl MismatchGroup {
    fn add(&mut self, m: Mismatch, examples: usize) {
        self.count += 1;
        self.first_height = min_opt(self.first_height, m.height);
        self.last_height = self.last_height.max(m.height);
        if self.examples.len() < examples {
            self.examples.push(m);
        }
    }
}

/// Results recorded under another identity or chain state, or invalidated, whose blocks no
/// current unit has re-checked. They are not counted in the totals, but are not forgotten
/// either: re-run (with `--resume`) to re-validate them.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Unverified {
    pub units: usize,
    pub blocks: u64,
    pub mismatches: u64,
    pub mismatches_by_key: BTreeMap<String, u64>,
}

/// Aggregated report over an output directory, for one identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Summary {
    /// The [`Identity::id`] the summary is for.
    pub identity: String,
    pub units: usize,
    pub totals: Stats,
    pub mismatches_by_key: BTreeMap<String, MismatchGroup>,
    /// Mismatches that fail a gate (all but [`INFORMATIONAL_CHECKS`]); never fewer than the
    /// progress counts say, even if the log lost records.
    pub failing: u64,
    /// How many of them say blocks may not have been checked ([`COVERAGE_CHECKS`]).
    #[serde(default)]
    pub incomplete: u64,
    /// `totals.mismatches` minus the records found for the counted runs; non-zero means the
    /// mismatch log lost records (it should always be 0).
    pub unaccounted: i64,
    pub unverified: Unverified,
    /// For a gate ([`summarize`] with requirements): the requirements no counted unit covers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<String>,
    /// The unit results counted, whose mismatches the summary holds (not written out).
    #[serde(skip)]
    pub counted: Vec<UnitDone>,
}

/// What an output directory holds. A directory is never mixed across networks, sources, RPC
/// chunk sizes or blocks directories, so unit groups always mean the same block ranges.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    pub net: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocks_dir: Option<String>,
}

/// Cuts a file back to its last complete line (an interrupted write leaves a partial one),
/// reading backwards from the end so a large log is not read whole.
fn repair_tail(path: &Path) -> Result<()> {
    use std::os::unix::fs::FileExt;
    let Ok(f) = OpenOptions::new().read(true).write(true).open(path) else {
        return Ok(());
    };
    let len = f.metadata()?.len();
    if len == 0 {
        return Ok(());
    }
    let mut end = len;
    let mut buf = vec![0u8; 64 * 1024];
    let keep = loop {
        if end == 0 {
            break 0;
        }
        let start = end.saturating_sub(buf.len() as u64);
        let chunk = &mut buf[..(end - start) as usize];
        f.read_exact_at(chunk, start)?;
        if end == len && chunk.last() == Some(&b'\n') {
            return Ok(());
        }
        if let Some(p) = chunk.iter().rposition(|b| *b == b'\n') {
            break start + p as u64 + 1;
        }
        end = start;
    };
    eprintln!("warning: dropping a partial last line of {}", path.display());
    f.set_len(keep)?;
    Ok(())
}

/// Thread-safe writer for an output directory.
pub struct Reporter {
    dir: PathBuf,
    mismatches: Mutex<File>,
    progress: Mutex<File>,
    examples_per_key: usize,
    /// Lines written per mismatch group in this run; beyond `cap` mismatches are counted, not
    /// written.
    written: Mutex<HashMap<String, u64>>,
    cap: u64,
    run: String,
    identity: String,
    policy: String,
    /// Units this run recorded as finished. Mismatch records are matched to their unit by
    /// unit and run, so a run records each unit at most once.
    recorded: Mutex<HashSet<String>>,
    /// Held for the reporter's lifetime so two processes never write one directory.
    _lock: File,
}

impl Reporter {
    /// Opens (or creates) an output directory for a run of `identity`. `cap` limits the lines
    /// written per mismatch group (0 = unlimited); suppressed mismatches are still counted in
    /// the summary.
    pub fn open(
        dir: &Path,
        meta: &Meta,
        identity: &Identity,
        examples_per_key: usize,
        cap: u64,
    ) -> Result<Self> {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let lock = File::create(dir.join("lock"))?;
        if lock.try_lock().is_err() {
            bail!("{} is in use by another harness process", dir.display());
        }
        let meta_path = dir.join("meta.json");
        if meta_path.exists() {
            let existing: Meta = serde_json::from_slice(&std::fs::read(&meta_path)?)
                .with_context(|| format!("parsing {}", meta_path.display()))?;
            if existing != *meta {
                bail!(
                    "{} holds results for {existing:?}, not {meta:?}; use another --out",
                    dir.display()
                );
            }
        } else {
            std::fs::write(&meta_path, serde_json::to_vec_pretty(meta)?)?;
        }
        for f in ["mismatches.ndjson", "progress.ndjson", "invalidated.ndjson", "runs.ndjson"] {
            repair_tail(&dir.join(f))?;
        }
        let started =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        let run =
            format!("{}.{:09}-{}", started.as_secs(), started.subsec_nanos(), std::process::id());
        let id = identity.id();
        let reporter = Self {
            dir: dir.to_path_buf(),
            mismatches: Mutex::new(open_append(dir, "mismatches.ndjson")?),
            progress: Mutex::new(open_append(dir, "progress.ndjson")?),
            examples_per_key,
            written: Mutex::new(HashMap::new()),
            cap,
            run: run.clone(),
            identity: id.clone(),
            policy: identity.policy_id(),
            recorded: Mutex::new(HashSet::new()),
            _lock: lock,
        };
        let record = RunRecord {
            run,
            identity: id,
            policy_id: identity.policy_id(),
            code: identity.code.clone(),
            oracle: identity.oracle.clone(),
            policy: identity.policy.clone(),
            base_sdk: identity.base_sdk.clone(),
        };
        append_line(&mut open_append(dir, "runs.ndjson")?, &record)?;
        Ok(reporter)
    }

    /// Units of this run's identity that earlier runs finished and nothing has invalidated.
    pub fn checkpoints(&self) -> Result<Vec<UnitDone>> {
        let invalid = read_invalidated(&self.dir)?;
        let mut out = Vec::new();
        for_each_line::<UnitDone>(&self.dir.join("progress.ndjson"), |u| {
            if is_current(&u, &self.identity, &invalid) {
                out.push(u);
            }
        })?;
        Ok(out)
    }

    /// Whether a valid checkpoint of this identity covers `coverage` of `unit`.
    pub fn is_covered(checkpoints: &[UnitDone], unit: &str, coverage: &Coverage) -> bool {
        checkpoints
            .iter()
            .any(|u| u.unit == unit && u.coverage.as_ref().is_some_and(|c| c.covers(coverage)))
    }

    /// Marks an earlier unit result as no longer valid.
    pub fn invalidate(&self, unit: &UnitDone, reason: &str) -> Result<()> {
        let record = Invalidated {
            unit: unit.unit.clone(),
            run: unit.run.clone(),
            by_run: self.run.clone(),
            reason: reason.to_string(),
        };
        append_line(&mut open_append(&self.dir, "invalidated.ndjson")?, &record)
    }

    /// Appends mismatches found by `unit`, and adds the ones suppressed by the cap to
    /// `stats.suppressed`.
    pub fn record_mismatches(
        &self,
        unit: &str,
        mismatches: &[Mismatch],
        stats: &mut Stats,
    ) -> Result<()> {
        if mismatches.is_empty() {
            return Ok(());
        }
        if self.recorded.lock().expect("recorded units poisoned").contains(unit) {
            bail!("unit {unit} was already recorded as finished by this run");
        }
        let mut buf = Vec::new();
        {
            let mut written = self.written.lock().expect("mismatch counts poisoned");
            for m in mismatches {
                let n = written.entry(m.key()).or_default();
                if self.cap > 0 && *n >= self.cap {
                    stats.suppressed.entry(m.key()).or_default().add(m.height);
                    continue;
                }
                *n += 1;
                let m = Mismatch {
                    unit: Some(unit.to_string()),
                    run: Some(self.run.clone()),
                    ..m.clone()
                };
                serde_json::to_writer(&mut buf, &m)?;
                buf.push(b'\n');
            }
        }
        let mut f = self.mismatches.lock().expect("mismatch log poisoned");
        f.write_all(&buf)?;
        Ok(())
    }

    /// Records a finished unit of this run.
    pub fn record_unit(
        &self,
        unit: &str,
        coverage: Coverage,
        stats: Stats,
        seconds: f64,
    ) -> Result<()> {
        if !self.recorded.lock().expect("recorded units poisoned").insert(unit.to_string()) {
            bail!("unit {unit} was already recorded as finished by this run");
        }
        // The unit's mismatch records reach the disk before the unit is recorded as done, so
        // a crash cannot leave a finished unit whose findings were lost.
        {
            let mut log = self.mismatches.lock().expect("mismatch log poisoned");
            log.flush()?;
            log.sync_data()?;
        }
        let unit = UnitDone {
            unit: unit.to_string(),
            run: self.run.clone(),
            identity: self.identity.clone(),
            policy: self.policy.clone(),
            coverage: Some(coverage),
            format: CHECKPOINT_FORMAT,
            stats,
            seconds,
        };
        append_line(&mut self.progress.lock().expect("progress log poisoned"), &unit)
    }

    /// Rebuilds `summary.json` (for this run's identity) from the logs.
    pub fn write_summary(&self) -> Result<Summary> {
        self.mismatches.lock().expect("mismatch log poisoned").flush()?;
        let summary = summarize(&self.dir, self.examples_per_key, Some(&self.identity), None)?;
        let f = File::create(self.dir.join("summary.json"))?;
        serde_json::to_writer_pretty(f, &summary)?;
        Ok(summary)
    }

    /// The gate over `scope`: every counted result of the units it names, and the
    /// requirements no counted unit covers.
    pub fn gate(&self, scope: &[Requirement]) -> Result<Summary> {
        self.mismatches.lock().expect("mismatch log poisoned").flush()?;
        summarize(&self.dir, self.examples_per_key, Some(&self.identity), Some(scope))
    }
}

fn open_append(dir: &Path, name: &str) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(name))
        .with_context(|| format!("opening {}", dir.join(name).display()))
}

fn append_line<T: Serialize>(f: &mut File, value: &T) -> Result<()> {
    let mut line = serde_json::to_vec(value)?;
    line.push(b'\n');
    f.write_all(&line)?;
    f.flush()?;
    Ok(())
}

fn read_invalidated(dir: &Path) -> Result<HashSet<(String, String)>> {
    let mut out = HashSet::new();
    for_each_line::<Invalidated>(&dir.join("invalidated.ndjson"), |i| {
        out.insert((i.unit, i.run));
    })?;
    Ok(out)
}

/// Whether `u` is a valid result of `identity`: recorded under it, in the current
/// [`CHECKPOINT_FORMAT`], with its coverage, and not invalidated since.
fn is_current(u: &UnitDone, identity: &str, invalid: &HashSet<(String, String)>) -> bool {
    u.identity == identity
        && u.format == CHECKPOINT_FORMAT
        && u.coverage.is_some()
        && !invalid.contains(&(u.unit.clone(), u.run.clone()))
}

/// The identity and policy id of `identity`'s runs, or of the latest run, in an output
/// directory.
fn run_identity(dir: &Path, identity: Option<&str>) -> Result<(String, String)> {
    let mut found = None;
    for_each_line::<RunRecord>(&dir.join("runs.ndjson"), |r| {
        if identity.is_none_or(|i| i == r.identity) {
            found = Some((r.identity, r.policy_id));
        }
    })?;
    Ok(found.unwrap_or_else(|| (identity.unwrap_or_default().to_string(), String::new())))
}

/// Builds a [`Summary`] of an output directory for `identity` (default: the latest run's).
///
/// Counted are the units of that identity that are still valid (not in `invalidated.ndjson`).
/// Within a unit group, a later unit replaces an earlier one only when its coverage includes
/// the earlier one's ([`Coverage::covers`]); otherwise both are counted, so a result is never
/// dropped for blocks that were not re-checked. Only the mismatch records of counted units
/// are counted (records of replaced or failed runs stay in the log but are ignored), plus the
/// mismatches the line cap suppressed. Results of other identities, or invalidated ones, that
/// no counted unit re-checks are reported under [`Summary::unverified`].
///
/// With `scope`, the summary is a gate: only the units of the scope's groups are counted, and
/// [`Summary::missing`] lists the requirements no counted unit covers.
pub fn summarize(
    dir: &Path,
    examples_per_key: usize,
    identity: Option<&str>,
    scope: Option<&[Requirement]>,
) -> Result<Summary> {
    let (identity, policy) = run_identity(dir, identity)?;
    let invalid = read_invalidated(dir)?;
    let in_scope = |group: &str| scope.is_none_or(|s| s.iter().any(|r| r.unit == group));
    let mut live: BTreeMap<String, Vec<UnitDone>> = BTreeMap::new();
    let mut old: Vec<UnitDone> = Vec::new();
    for_each_line::<UnitDone>(&dir.join("progress.ndjson"), |u| {
        if !in_scope(u.group()) {
            return;
        }
        if !is_current(&u, &identity, &invalid) {
            old.push(u);
            return;
        }
        let group = live.entry(u.group().to_string()).or_default();
        let cov = u.coverage.as_ref().expect("checked");
        group.retain(|prev| !cov.covers(prev.coverage.as_ref().expect("live units have one")));
        group.push(u);
    })?;
    // An old result is re-checked by a current unit covering its blocks under the same policy
    // (a weaker policy must not clear a stronger one's findings). Records that predate
    // identities are replaced by any current unit.
    old.retain(|o| {
        let same_policy = o.policy == policy || o.coverage.is_none();
        !(same_policy
            && live.get(o.group()).is_some_and(|units| {
                units
                    .iter()
                    .any(|u| u.coverage.as_ref().expect("live").rechecks(o.coverage.as_ref()))
            }))
    });

    let mut totals = Stats::default();
    for u in live.values().flatten() {
        totals.merge(&u.stats);
    }
    let mut unverified = Unverified {
        units: old.len(),
        ..Default::default()
    };
    for u in &old {
        unverified.blocks += u.stats.blocks;
        for (k, s) in &u.stats.suppressed {
            *unverified.mismatches_by_key.entry(k.clone()).or_default() += s.count;
        }
    }
    let counted: HashSet<(&str, &str)> =
        live.values().flatten().map(|u| (u.unit.as_str(), u.run.as_str())).collect();
    let stale: HashSet<(&str, &str)> =
        old.iter().map(|u| (u.unit.as_str(), u.run.as_str())).collect();
    let mut groups: BTreeMap<String, MismatchGroup> = BTreeMap::new();
    for_each_line::<Mismatch>(&dir.join("mismatches.ndjson"), |m| {
        let (Some(unit), Some(run)) = (m.unit.as_deref(), m.run.as_deref()) else {
            return;
        };
        if counted.contains(&(unit, run)) {
            groups.entry(m.key()).or_default().add(m, examples_per_key);
        } else if stale.contains(&(unit, run)) {
            *unverified.mismatches_by_key.entry(m.key()).or_default() += 1;
        }
    })?;
    unverified.mismatches = unverified.mismatches_by_key.values().sum();
    for (k, sup) in &totals.suppressed {
        let g = groups.entry(k.clone()).or_default();
        g.count += sup.count;
        g.first_height = min_opt(g.first_height, sup.first_height);
        g.last_height = g.last_height.max(sup.last_height);
    }
    for g in groups.values_mut() {
        g.examples.sort_by_key(|m| (m.height, m.tx_index));
    }
    let found: u64 = groups.values().map(|g| g.count).sum();
    let check_of = |k: &str| k.split(':').next().unwrap_or(k).to_string();
    let found_failing: u64 = groups
        .iter()
        .filter(|(k, _)| !INFORMATIONAL_CHECKS.contains(&check_of(k).as_str()))
        .map(|(_, g)| g.count)
        .sum();
    // Fail closed: the progress counts are authoritative if the log lost records.
    let failing = found_failing.max(totals.mismatches.saturating_sub(totals.informational));
    let incomplete = groups
        .iter()
        .filter(|(k, _)| COVERAGE_CHECKS.contains(&check_of(k).as_str()))
        .map(|(_, g)| g.count)
        .sum();
    let unaccounted = totals.mismatches as i64 - found as i64;
    if unaccounted != 0 {
        eprintln!("warning: {unaccounted} mismatches counted in progress but missing from the log");
    }
    let mut missing: Vec<String> = scope
        .unwrap_or_default()
        .iter()
        .filter(|r| {
            !live.get(&r.unit).is_some_and(|units| {
                units.iter().any(|u| u.coverage.as_ref().expect("live").covers(&r.coverage))
            })
        })
        .map(|r| format!("{} {:?}", r.unit, r.coverage))
        .collect();
    if scope.is_some() && unaccounted != 0 {
        missing.push(format!("{unaccounted} mismatch records missing from mismatches.ndjson"));
    }
    let counted: Vec<UnitDone> = live.into_values().flatten().collect();
    Ok(Summary {
        identity,
        units: counted.len(),
        counted,
        totals,
        mismatches_by_key: groups,
        failing,
        incomplete,
        unaccounted,
        unverified,
        missing,
    })
}

/// Calls `f` for every record of an NDJSON file. A truncated *last* line (an interrupted
/// write) is skipped with a warning; any other bad line is an error.
fn for_each_line<T: for<'de> Deserialize<'de>>(path: &Path, mut f: impl FnMut(T)) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let mut lines = BufReader::new(File::open(path)?).lines().peekable();
    while let Some(line) = lines.next() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str(&line) {
            Ok(v) => f(v),
            Err(e) if lines.peek().is_none() => {
                eprintln!("warning: ignoring truncated last line of {}: {e}", path.display());
            }
            Err(e) => return Err(e).with_context(|| format!("bad line in {}", path.display())),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mismatch(height: u32, check: &str) -> Mismatch {
        Mismatch {
            net: "testnet".into(),
            source: "rpc".into(),
            height: Some(height),
            block_hash: None,
            tx_index: None,
            txid: None,
            check: check.into(),
            side: Side::Dashcore,
            field: None,
            expected: None,
            actual: None,
            detail: String::new(),
            tx_hex: None,
            unit: None,
            run: None,
        }
    }

    fn meta(net: &str) -> Meta {
        Meta {
            net: net.into(),
            source: "rpc".into(),
            chunk: Some(10),
            blocks_dir: None,
        }
    }

    fn identity(policy: &str) -> Identity {
        Identity::of_build(policy)
    }

    fn heights(lo: u32, hi: u32) -> Coverage {
        Coverage::Heights {
            lo,
            hi,
            prev_hash: None,
            end_hash: format!("h{hi}"),
        }
    }

    /// Records a unit with one `tx.decode` finding per height in `bad`.
    fn unit(r: &Reporter, group: &str, cov: Coverage, blocks: u64, bad: &[u32]) {
        let mut stats = Stats {
            blocks,
            ..Default::default()
        };
        let found: Vec<_> = bad.iter().map(|h| mismatch(*h, "tx.decode")).collect();
        stats.count(&found);
        r.record_mismatches(group, &found, &mut stats).unwrap();
        r.record_unit(group, cov, stats, 1.0).unwrap();
    }

    fn count(s: &Summary) -> u64 {
        s.mismatches_by_key.get("tx.decode:dashcore").map_or(0, |g| g.count)
    }

    #[test]
    fn coverage_relation() {
        assert!(heights(0, 9).covers(&heights(0, 4)));
        assert!(heights(0, 9).covers(&heights(0, 9)));
        assert!(!heights(0, 4).covers(&heights(5, 9)), "disjoint ranges of one chunk");
        assert!(!heights(5, 9).covers(&heights(0, 9)));
        let other_chain = Coverage::Heights {
            lo: 0,
            hi: 9,
            prev_hash: None,
            end_hash: "x".into(),
        };
        assert!(!heights(0, 9).covers(&other_chain), "same range, other chain");
        let sel = |s: &str| Coverage::Selection {
            sig: s.into(),
        };
        assert!(sel("a").covers(&sel("a")) && !sel("a").covers(&sel("b")));
        assert!(Coverage::Index.covers(&Coverage::Index) && !Coverage::Index.covers(&sel("a")));
    }

    #[test]
    fn summary_merges_by_coverage_not_by_count() {
        let dir = tempfile::tempdir().unwrap();
        let id = identity("p");
        // Each unit in its own run, recording one unit and returning the summary.
        let run = |cov: Coverage, blocks: u64, bad: &[u32]| {
            let r = Reporter::open(dir.path(), &meta("testnet"), &id, 20, 0).unwrap();
            assert!(Reporter::open(dir.path(), &meta("testnet"), &id, 2, 0).is_err(), "locked");
            unit(&r, "rpc:0", cov, blocks, bad);
            r.write_summary().unwrap()
        };
        // Review r1 finding 3: five findings in 0..=4, then a clean 5..=9 of the same chunk
        // with as many blocks; the clean unit must not erase the first one's findings.
        run(heights(0, 4), 5, &[0, 1, 2, 3, 4]);
        let s = run(heights(5, 9), 5, &[]);
        assert_eq!((count(&s), s.units, s.totals.blocks), (5, 2, 10));
        // A failing narrow re-check stays visible next to the wider clean unit.
        let s = run(heights(6, 6), 1, &[6]);
        assert_eq!((count(&s), s.units), (6, 3));
        // A unit covering all of them replaces them.
        let s = run(heights(0, 9), 10, &[]);
        assert_eq!((count(&s), s.units, s.totals.blocks, s.unaccounted), (0, 1, 10, 0));
        // ...and so does the same range again (the latest result wins).
        assert_eq!(count(&run(heights(0, 9), 10, &[3])), 1);
    }

    #[test]
    fn checkpoints_are_bound_to_identity_and_validity() {
        let dir = tempfile::tempdir().unwrap();
        let a = identity("policy a");
        let r = Reporter::open(dir.path(), &meta("testnet"), &a, 20, 0).unwrap();
        unit(&r, "rpc:0", heights(0, 9), 10, &[2]);
        unit(&r, "rpc:10", heights(10, 19), 10, &[]);
        let cps = r.checkpoints().unwrap();
        assert!(Reporter::is_covered(&cps, "rpc:0", &heights(0, 9)));
        assert!(Reporter::is_covered(&cps, "rpc:0", &heights(3, 5)), "a superset covers");
        assert!(!Reporter::is_covered(&cps, "rpc:0", &heights(0, 12)));
        assert!(!Reporter::is_covered(&cps, "rpc:10", &heights(0, 9)), "other group");
        // A reorg invalidates rpc:10: no longer a checkpoint, no longer counted.
        let rpc10 = cps.iter().find(|u| u.unit == "rpc:10").unwrap().clone();
        r.invalidate(&rpc10, "reorg").unwrap();
        assert_eq!(r.checkpoints().unwrap().len(), 1);
        let s = r.write_summary().unwrap();
        assert_eq!((s.units, s.totals.blocks, s.unverified.units), (1, 10, 1));
        drop(r);

        // Another identity (here another oracle; equally another build or policy) reuses
        // nothing and counts nothing of the first one, but its finding stays visible as
        // unverified until a unit of the same policy re-checks those blocks.
        let b = identity("policy a").with_oracle("another oracle");
        let r = Reporter::open(dir.path(), &meta("testnet"), &b, 20, 0).unwrap();
        assert!(r.checkpoints().unwrap().is_empty());
        let s = r.write_summary().unwrap();
        assert_eq!((s.units, count(&s)), (0, 0));
        assert_eq!((s.unverified.units, s.unverified.mismatches), (2, 1));
        unit(&r, "rpc:0", heights(0, 9), 10, &[]);
        let s = r.write_summary().unwrap();
        assert_eq!((s.units, s.unverified.units, s.unverified.mismatches), (1, 1, 0));
        drop(r);
        // `summarize` without an identity reports the latest run's.
        assert_eq!(summarize(dir.path(), 5, None, None).unwrap().identity, b.id());
    }

    #[test]
    fn checkpoints_of_another_format_are_not_reused() {
        // A format-1 checkpoint (no `format`, no `prev_hash`) of the same identity is neither
        // reused nor counted: it is unverified, and its finding stays visible.
        let dir = tempfile::tempdir().unwrap();
        let a = identity("p");
        let r = Reporter::open(dir.path(), &meta("testnet"), &a, 20, 0).unwrap();
        unit(&r, "rpc:0", heights(0, 9), 10, &[2]);
        assert_eq!(r.checkpoints().unwrap().len(), 1);
        drop(r);
        let path = dir.path().join("progress.ndjson");
        let mut line: serde_json::Value =
            serde_json::from_str(std::fs::read_to_string(&path).unwrap().trim()).unwrap();
        assert_eq!(line["format"], CHECKPOINT_FORMAT);
        line.as_object_mut().unwrap().remove("format");
        line["coverage"].as_object_mut().unwrap().remove("prev_hash");
        std::fs::write(&path, format!("{line}\n")).unwrap();

        let r = Reporter::open(dir.path(), &meta("testnet"), &a, 20, 0).unwrap();
        assert!(r.checkpoints().unwrap().is_empty());
        let s = r.write_summary().unwrap();
        assert_eq!((s.units, s.unverified.units, s.unverified.mismatches), (0, 1, 1));
        let scope = [Requirement {
            unit: "rpc:0".into(),
            coverage: heights(0, 9),
        }];
        assert_eq!(r.gate(&scope).unwrap().missing.len(), 1, "fails closed");
    }

    #[test]
    fn gate_fails_closed_when_the_log_lost_records() {
        // Review r2: a unit's progress line says it found a mismatch, but the record is gone
        // (crash between the two files, or a truncated log).
        let dir = tempfile::tempdir().unwrap();
        let id = identity("p");
        let r = Reporter::open(dir.path(), &meta("testnet"), &id, 20, 0).unwrap();
        unit(&r, "rpc:0", heights(0, 9), 10, &[4]);
        drop(r);
        std::fs::write(dir.path().join("mismatches.ndjson"), "").unwrap();
        let r = Reporter::open(dir.path(), &meta("testnet"), &id, 20, 0).unwrap();
        let g = r
            .gate(&[Requirement {
                unit: "rpc:0".into(),
                coverage: heights(0, 9),
            }])
            .unwrap();
        assert_eq!((g.failing, g.unaccounted, g.missing.len()), (1, 1, 1), "{:?}", g.missing);
    }

    #[test]
    fn a_weaker_policy_does_not_clear_a_stronger_ones_findings() {
        // Review r2: findings of a policy with more oracle checks stay unverified until a run
        // of that policy re-checks the blocks.
        let dir = tempfile::tempdir().unwrap();
        let strong = identity("verbose_every=1");
        let r = Reporter::open(dir.path(), &meta("testnet"), &strong, 20, 0).unwrap();
        unit(&r, "rpc:0", heights(0, 9), 10, &[3, 7]);
        drop(r);
        let weak = identity("verbose_every=0");
        let r = Reporter::open(dir.path(), &meta("testnet"), &weak, 20, 0).unwrap();
        unit(&r, "rpc:0", heights(0, 9), 10, &[]);
        let s = r.write_summary().unwrap();
        assert_eq!((s.failing, s.unverified.units, s.unverified.mismatches), (0, 1, 2));
        drop(r);
        // The strong policy re-checking the blocks clears them.
        let r = Reporter::open(dir.path(), &meta("testnet"), &strong, 20, 0).unwrap();
        unit(&r, "rpc:0", heights(0, 9), 10, &[]);
        let s = r.write_summary().unwrap();
        assert_eq!((s.failing, s.unverified.units), (0, 1), "the weak run is now unverified");
    }

    #[test]
    fn identity_covers_code_oracle_and_policy() {
        let a = identity("p");
        assert_ne!(a.id(), identity("q").id());
        assert_ne!(a.id(), a.clone().with_oracle("/Dash Core:24.0.0/ 240000").id());
        assert_eq!(a.policy_id(), a.clone().with_oracle("x").policy_id());
        assert!(a.code.starts_with(env!("CONFORMANCE_CODE_ID")), "source identity");
        assert!(a.code.len() > env!("CONFORMANCE_CODE_ID").len() + 16, "executable digest");
    }

    #[test]
    fn gate_counts_every_unit_in_scope_whichever_run_checked_it() {
        let dir = tempfile::tempdir().unwrap();
        let id = identity("p");
        let r = Reporter::open(dir.path(), &meta("testnet"), &id, 20, 0).unwrap();
        unit(&r, "rpc:0", heights(0, 9), 10, &[4]);
        drop(r);
        // A resumed run checks nothing new: the gate still sees the finding (review r1
        // finding 1).
        let r = Reporter::open(dir.path(), &meta("testnet"), &id, 20, 0).unwrap();
        let scope = |unit: &str, cov: Coverage| Requirement {
            unit: unit.into(),
            coverage: cov,
        };
        let g = r.gate(&[scope("rpc:0", heights(0, 9))]).unwrap();
        assert_eq!((g.failing, g.missing.len()), (1, 0));
        // Units outside the scope don't count; requirements nothing covers are listed.
        unit(&r, "rpc:20", heights(20, 29), 10, &[25]);
        let g = r.gate(&[scope("rpc:0", heights(0, 9)), scope("rpc:10", heights(10, 19))]).unwrap();
        assert_eq!((g.failing, g.missing.len()), (1, 1));
        let other_chain = Coverage::Heights {
            lo: 0,
            hi: 9,
            prev_hash: None,
            end_hash: "reorged".into(),
        };
        assert_eq!(r.gate(&[scope("rpc:0", other_chain)]).unwrap().missing.len(), 1);
    }

    #[test]
    fn cap_suppression_torn_lines_and_legacy_records() {
        let dir = tempfile::tempdir().unwrap();
        let id = identity("p");
        let r = Reporter::open(dir.path(), &meta("testnet"), &id, 2, 3).unwrap();
        // Five findings, three written; the cap (per group and run) counts the other two
        // with their heights.
        let mut s2 = Stats {
            blocks: 12,
            ..Default::default()
        };
        let found: Vec<_> = [9, 10, 11, 12, 13].iter().map(|h| mismatch(*h, "tx.decode")).collect();
        s2.count(&found);
        r.record_mismatches("blk:7", &found, &mut s2).unwrap();
        let sup = &s2.suppressed["tx.decode:dashcore"];
        assert_eq!((sup.count, sup.first_height, sup.last_height), (2, Some(12), Some(13)));
        let sel = Coverage::Selection {
            sig: "b".into(),
        };
        r.record_unit("blk:7", sel, s2, 1.0).unwrap();
        // A unit that failed (never recorded) is ignored.
        r.record_mismatches("blk:8", &[mismatch(50, "block.hash")], &mut Stats::default()).unwrap();
        let s = r.write_summary().unwrap();
        let g = &s.mismatches_by_key["tx.decode:dashcore"];
        assert_eq!(g.count, 5);
        assert_eq!((g.first_height, g.last_height, g.examples.len()), (Some(9), Some(13), 2));
        assert!(!s.mismatches_by_key.contains_key("block.hash:dashcore"));
        assert_eq!((s.failing, s.unaccounted), (5, 0));
        drop(r);

        // A torn last line is cut back when the directory is reopened, so later appends work;
        // the cut is found from the end of a log larger than one read window.
        let big = "x".repeat(100 * 1024);
        for f in ["progress.ndjson", "mismatches.ndjson"] {
            let mut f = OpenOptions::new().append(true).open(dir.path().join(f)).unwrap();
            f.write_all(format!("{{\"unit\":\"blk:9@{big}").as_bytes()).unwrap();
        }
        assert_eq!(summarize(dir.path(), 1, None, None).unwrap().units, 1, "tolerated when last");
        let empty = dir.path().join("empty.ndjson");
        std::fs::write(&empty, "").unwrap();
        repair_tail(&empty).unwrap();
        let r = Reporter::open(dir.path(), &meta("testnet"), &id, 2, 0).unwrap();
        r.record_unit("blk:9", Coverage::Index, Stats::default(), 1.0).unwrap();
        assert!(r.record_unit("blk:9", Coverage::Index, Stats::default(), 1.0).is_err(), "once");
        assert_eq!(r.write_summary().unwrap().units, 2);
        drop(r);

        // Directories are never reused for other networks or chunk sizes.
        assert!(Reporter::open(dir.path(), &meta("mainnet"), &id, 2, 0).is_err());
        let other_chunk = Meta {
            chunk: Some(500),
            ..meta("testnet")
        };
        assert!(Reporter::open(dir.path(), &other_chunk, &id, 2, 0).is_err());

        // Records written before identities existed are neither reused nor counted.
        let old = tempfile::tempdir().unwrap();
        std::fs::write(old.path().join("progress.ndjson"), "{\"unit\":\"rpc:0@0-9\",\"run\":\"1\",\"stats\":{\"blocks\":10,\"stale_blocks\":0,\"bytes\":0,\"txs\":0,\"special_txs\":{},\"verbose_blocks\":0,\"oracle_hash_checks\":0,\"mismatches\":0,\"min_height\":null,\"max_height\":null},\"seconds\":1.0}\n").unwrap();
        let r = Reporter::open(old.path(), &meta("testnet"), &id, 2, 0).unwrap();
        assert!(r.checkpoints().unwrap().is_empty());
        let s = r.write_summary().unwrap();
        assert_eq!((s.units, s.unverified.units), (0, 1));
        unit(&r, "rpc:0", heights(0, 9), 10, &[]);
        assert_eq!(r.write_summary().unwrap().unverified.units, 0, "re-checked");
    }
}
