//! L2 (CH3a a7-2; CH3 §7.2): a chain window through V and the `CoreConnector` leg of
//! [`super::replay_core`], as `digest l2` runs it, with what a7-1's replay lacks.
//!
//! - **Input.** A Core `blocks/` directory's best chain, heights `0..=N`, through [`BlockDir`]
//!   (CH3a D150), read ahead on a thread of its own so that V time is V's. Or, the node form, a
//!   stored node's `blocks.dat` with its recorded decisions, as `replay_node` replays it (read on
//!   the feeding thread, so its V time includes the reading).
//! - **assumevalid** (CH3 D44; CH3a D153). Off is `-assumevalid=0`. On, the best chain's headers to
//!   the directory's tip (CH3a D162: or a [`HeaderFile`]'s chain, which must hold the window's
//!   blocks, reach the directory's tip, hold `defaultAssumeValid` unless it is null and meet the
//!   minimum chain work), genesis excluded, go through `process_headers` first, in batches of
//!   [`MAX_HEADERS_RESULTS`] with their hashes; a batch that stops, or whose last accepted header
//!   is not its last, fails the run. assumevalid is the chain's `defaultAssumeValid` when the
//!   preloaded chain holds it, else the preloaded tip. `-minimumchainwork` stays at the chain's
//!   default when the tip's work meets it, else it is the tip's work. The mock clock starts at the
//!   later of the window's last block and the preloaded tip, so that no header is from the future.
//!   Both choices go in the manifest with their reasons.
//! - **The tally.** Each attempt the connector records is counted: `C`s by `script_checks` (CH3a
//!   D151), in runs of heights; other connects; `D`s by result.
//! - **The reverse leg** (CH3 D48; CH3a D152). After the forward run, `invalidate_block` of block
//!   L+1 makes V disconnect its tip down to L, each block through `DisconnectBlock`, and run ABC.
//!   Its `D`s go to a second log ([`reverse_path`]) with an `H` whose run is `<run>-reverse`: in
//!   the forward log they would end its final chain at L. A [`RewindObserver`] compares the
//!   accumulator after each clean rewind with the parent's golden `muhash`, `txouts` and
//!   `amount`, and the first mismatch stops V with the height and the key. There must be exactly
//!   tip − L `D`s, all `ok`, and nothing connected.
//! - **The end check.** After V stops, a scan of every coin in the cache (none was flushed:
//!   `NoFlush`, and no frozen layer) must give golden(L)'s `muhash`, `txouts` and `amount`. It
//!   tests `ApplyTxInUndo`'s restored coins rather than the accumulator's arithmetic.
//! - **The suffix check** (CH3 R11; CH3a D165-D170). `--start-utxo` starts V as a pruned node at
//!   a [`super::utxo`] snapshot's base B: the index of the header chain's blocks `0..=B` without
//!   data (each `nTx` from the golden), `m_have_pruned` set, and the snapshot's coins at B. The
//!   snapshot must match its sidecar, golden(B) and the header chain at B. Before V starts, a scan
//!   of the loaded coins must give golden(B)'s `muhash`, `txouts` and `amount`, and the log's
//!   accumulator is seeded from that scan. Blocks `B+1..=N` then go through V. The log finalizes
//!   the MuHash at N and at L only, so a forward `C` holds `muhash` at N, and at L when L > B.
//!   After the run, each such `C` must have the golden's block, `muhash`, `txouts`, `amount` and
//!   `unspendable`, and the reverse leg compares `muhash` at L only.
//! - **`--save-utxo`** writes V's coins at the window's end N as a snapshot, with its sidecar,
//!   once a scan of them gives golden(N)'s three values.
//! - **The manifest** (CH3a D154): the input, the window, the choices, `-par`, the cache, the
//!   tallies, the reverse leg, the start, the forward log's checks against the golden
//!   (`forward_golden`) and the saved snapshot, the timings, the host and the binary (`exe`, its
//!   sha256; `git` is the source tree's state at the run's start). V time (R11's figure, CH3a
//!   D155) is the wall time from the first `process_block` sent to the last reply.
//!
//! A run refuses an output that exists before V starts. A failed run removes each file it created,
//! as `digest replay` removes its logs: its logs, a snapshot with its sidecar, and a manifest it
//! could not finish.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::sync_channel;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, anyhow, bail, ensure};
use dash_chain::{BlockId, BlockIndexRecord, ChainConfig, HeaderBatch, HeaderStop, Status, Work};
use dash_consensus::coins::stats::{CoinStatsAccumulator, CoinStatsTotals};
use dash_consensus::coins::{Coin, CoinRef, MuHash3072};
use dash_consensus::pow::block_proof;
use dash_num::CompactTarget;
use dash_params::ChainParams;
use dash_primitives::BlockHash;
use dash_statedigest::decisions::Policy;
use dash_statedigest::{Chain, Hash256, Header, Kind, Origin, Record, Value};
use dash_types::Numeric as _;
use dashd_chainstate::chain::{ConnectCx, ConnectOutcome, DisconnectResult, Fatal, genesis_block};
use dashd_chainstate::checkqueue::{CheckQueue, MAX_SCRIPTCHECK_THREADS, script_check_threads};
use dashd_chainstate::coins::{CoinLayerOps as _, OutPointKey};
use dashd_chainstate::connect::{
    DigestAttempt, DigestLog, DigestSink, IbdHelper, MissSlot, MuHashAt, RewindObserver, Rewound,
};
use serde_json::{Value as Json, json};

use super::blkdir::BlockDir;
use super::frames::Sha256Reader;
use super::headerfile::HeaderFile;
use super::replay::{create_new, display};
use super::replay_core::{
    Coins, FeedStart, Leg, MakeSink, NodeBlocks, Pruned, Rig, Settings, Start, Step, check_tip,
    feed, header, node_leg,
};
use super::store::{Node, Records};
use super::utxo::{self, sidecar_path};

/// `MAX_HEADERS_UNCOMPRESSED_RESULT`, Dash's `MAX_HEADERS_RESULTS`: a `headers` message's most.
// Core: validation.h:82; net_processing.cpp:1210-1216 (GetHeadersLimit)
pub const MAX_HEADERS_RESULTS: usize = 2_000;

/// The least `-par` that gives the most threads: the main one and `MAX_SCRIPTCHECK_THREADS` more.
// Core: validation.h:77; init.cpp:1554-1565
pub const MAX_PAR: i64 = MAX_SCRIPTCHECK_THREADS as i64 + 1;

/// Blocks the reader thread holds ready.
const READ_AHEAD: usize = 4;

/// How the run goes.
#[derive(Clone, Debug)]
pub struct Options {
    /// Preload the headers and skip scripts as `-assumevalid` would (a blocks directory only).
    pub assume_valid: bool,
    /// `-par`, read as the node reads it (`script_check_threads` on the online processors):
    /// 0 is one fewer than the processors, `-n` leaves `n` free (CH3a a9).
    pub par: i64,
    /// The coins cache's share, in bytes.
    pub coins_cache: u64,
    /// The reverse leg's end, at or below the window's.
    pub reverse_to: Option<u32>,
    /// The golden the reverse leg and the end check compare with.
    pub golden: Option<PathBuf>,
    /// Hash the input files into the manifest.
    pub sha256: bool,
    /// With `assume_valid`, preload this header file's chain rather than the blocks directory's
    /// (CH3a D162).
    pub headers: Option<PathBuf>,
    /// Write V's coins at the window's end to this snapshot, a new file, and its sidecar (CH3a
    /// D165). Needs `golden`.
    pub save_utxo: Option<PathBuf>,
    /// Start V as a pruned node at this snapshot's base (CH3a D165): a blocks directory, a window
    /// past the base, and `golden`.
    pub start_utxo: Option<PathBuf>,
}

/// What the run reads.
pub enum Source<'a> {
    /// A blocks directory's best chain to height `to`, on `params` of `chain`.
    Dir {
        dir: &'a BlockDir,
        params: Arc<ChainParams>,
        chain: Chain,
        to: u32,
    },
    /// A stored node, with its decisions under `policy`.
    Node {
        node: &'a Node,
        policy: Policy,
    },
}

/// What a run did.
#[derive(Debug)]
pub struct L2 {
    /// V's tip after the forward run: height and hash.
    pub tip: (u32, Hash256),
    /// The forward run's attempts.
    pub forward: Tally,
    pub reverse: Option<Reversed>,
    /// A pruned start's check: the scan of the coins it loaded, before V started.
    pub start: Option<EndCheck>,
    pub manifest: Json,
}

/// The reverse leg's result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reversed {
    pub to: u32,
    /// The leg's attempts.
    pub tally: Tally,
    /// Rewinds the observer compared with the golden.
    pub compared: u64,
    /// The coins scan after the leg.
    pub end: EndCheck,
}

/// The coins set, from a scan of every coin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndCheck {
    pub muhash: Hash256,
    pub txouts: u64,
    pub amount: i64,
}

/// The attempts a connector recorded.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    /// `C`s: connects with `fJustCheck == false`.
    pub connects: u64,
    /// `C`s whose scripts were skipped, checked, or never decided (genesis).
    pub skipped: u64,
    pub checked: u64,
    pub undecided: u64,
    /// Other connect attempts (`R`s).
    pub rejects: u64,
    /// `D`s, and those `ok`.
    pub disconnects: u64,
    pub disconnects_ok: u64,
    /// The `C`s in runs of consecutive heights with one `script_checks`: `(from, to, decision)`.
    pub runs: Vec<(u32, u32, Option<bool>)>,
}

impl Tally {
    fn count(&mut self, attempt: &DigestAttempt<'_>) {
        match attempt {
            DigestAttempt::Connect(c)
                if !c.just_check && matches!(c.outcome, Ok(ConnectOutcome::Connected)) =>
            {
                self.connects += 1;
                match c.script_checks {
                    Some(false) => self.skipped += 1,
                    Some(true) => self.checked += 1,
                    None => self.undecided += 1,
                }
                self.run(c.at.height, c.script_checks);
            }
            DigestAttempt::Connect(_) => self.rejects += 1,
            DigestAttempt::Disconnect(d) => {
                self.disconnects += 1;
                if matches!(d.outcome, Ok(DisconnectResult::Ok)) {
                    self.disconnects_ok += 1;
                }
            }
        }
    }

    /// Extends the last run with `h`, or starts a new one.
    fn run(&mut self, h: u32, decision: Option<bool>) {
        match self.runs.last_mut() {
            Some((_, to, d)) if *d == decision && *to + 1 == h => *to = h,
            _ => self.runs.push((h, h, decision)),
        }
    }

    fn to_json(&self) -> Json {
        let decision = |d: &Option<bool>| match d {
            Some(false) => "skip",
            Some(true) => "check",
            None => "none",
        };
        let runs: Vec<Json> =
            self.runs.iter().map(|(f, t, d)| json!([f, t, decision(d)])).collect();
        json!({
            "c": self.connects,
            "skipped": self.skipped,
            "checked": self.checked,
            "undecided": self.undecided,
            "r": self.rejects,
            "d": self.disconnects,
            "d_ok": self.disconnects_ok,
            "runs": runs,
        })
    }
}

/// The file the connector's log writes to: the forward log, then the reverse leg's.
#[derive(Clone)]
struct Switch(Arc<Mutex<File>>);

impl Switch {
    fn file(&self) -> std::io::Result<std::sync::MutexGuard<'_, File>> {
        self.0.lock().map_err(|_| std::io::Error::other("the log lock is poisoned"))
    }
}

impl Write for Switch {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.file()?.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file()?.flush()
    }
}

/// [`DigestLog`], with every attempt counted first.
struct Tallied {
    log: DigestLog<Switch>,
    tally: Arc<Mutex<Tally>>,
}

impl DigestSink for Tallied {
    fn record(&mut self, cx: &ConnectCx<'_>, attempt: &DigestAttempt<'_>) -> Result<(), Fatal> {
        self.tally.lock().map_err(|_| Fatal::new("l2: the tally lock is poisoned"))?.count(attempt);
        self.log.record(cx, attempt)
    }
}

/// A golden `C`'s block and the keys the reverse leg, the end checks and a pruned start read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Golden {
    pub b: Hash256,
    pub muhash: Hash256,
    pub txouts: u64,
    pub amount: i64,
    pub unspendable: i64,
}

/// The golden's tip `C`s of heights `from..=to`, one per height: a golden is linear.
pub fn golden_range(path: &Path, from: u32, to: u32) -> Result<BTreeMap<u32, Golden>> {
    Ok(read_golden(path, from, to, None)?.0)
}

/// [`golden_range`], with `ntx_to` also the `ntx` of each height `0..=ntx_to`, in one pass.
fn read_golden(
    path: &Path,
    from: u32,
    to: u32,
    ntx_to: Option<u32>,
) -> Result<(BTreeMap<u32, Golden>, Vec<u32>)> {
    let name = path.display();
    let mut out = BTreeMap::new();
    let mut ntx = vec![None; ntx_to.map_or(0, |n| n as usize + 1)];
    for record in Records::open(path)? {
        let (line, record) = record?;
        let Record::Outcome(o) = record else {
            continue;
        };
        let Ok(h) = u32::try_from(o.id.h) else {
            continue;
        };
        if o.kind() != Kind::Connect || o.id.org != Origin::Tip {
            continue;
        }
        let s = o.s();
        let key = |k: &str| s.get(k).with_context(|| format!("{name}:{line}: no {k} at {h}"));
        let int = |k: &str| {
            key(k)?.as_int().with_context(|| format!("{name}:{line}: {k} at {h} is not an integer"))
        };
        if let Some(slot) = ntx.get_mut(h as usize) {
            let n = u32::try_from(int("ntx")?)
                .with_context(|| format!("{name}:{line}: ntx at {h} is not a count"))?;
            ensure!(slot.replace(n).is_none(), "{name}:{line}: a second C at {h}");
        }
        if !(from..=to).contains(&h) {
            continue;
        }
        let muhash = match key("muhash")? {
            Value::Str(hex) => Hash256::from_hex(hex),
            _ => None,
        }
        .with_context(|| format!("{name}:{line}: muhash at {h} is not a hash"))?;
        let golden = Golden {
            b: o.id.b,
            muhash,
            txouts: u64::try_from(int("txouts")?)
                .with_context(|| format!("{name}:{line}: txouts at {h} is negative"))?,
            amount: int("amount")?,
            unspendable: int("unspendable")?,
        };
        ensure!(out.insert(h, golden).is_none(), "{name}:{line}: a second C at {h}");
    }
    if let Some(h) = (from..=to).find(|h| !out.contains_key(h)) {
        bail!("{name}: no C at {h}");
    }
    let ntx = ntx
        .into_iter()
        .enumerate()
        .map(|(h, n)| n.with_context(|| format!("{name}: no C at {h}")))
        .collect::<Result<_>>()?;
    Ok((out, ntx))
}

/// The first of the block, `muhash` (if `to` has one), `txouts` and `amount` at `to.height` that
/// is not `g`'s.
///
/// # Errors
///
/// The mismatch, with the height and the key.
pub fn compare_rewound(to: &Rewound, g: &Golden) -> Result<(), String> {
    let h = to.height;
    let differ = |key: &str, have: String, want: String| {
        Err(format!("reverse leg: at {h}, {key} is {have}, golden({h}) has {want}"))
    };
    if to.hash != g.b {
        return differ("the block", to.hash.to_hex(), g.b.to_hex());
    }
    if let Some(muhash) = to.muhash
        && muhash != g.muhash
    {
        return differ("muhash", muhash.to_hex(), g.muhash.to_hex());
    }
    if to.txouts != g.txouts {
        return differ("txouts", to.txouts.to_string(), g.txouts.to_string());
    }
    if to.amount != g.amount {
        return differ("amount", to.amount.to_string(), g.amount.to_string());
    }
    Ok(())
}

/// What the reverse leg's observer found.
#[derive(Default)]
struct Compared {
    count: u64,
    /// Rewinds that carried a `muhash`.
    muhash: u64,
    mismatch: Option<String>,
}

/// Compares each rewind with the golden.
struct GoldenCheck {
    want: Arc<BTreeMap<u32, Golden>>,
    seen: Arc<Mutex<Compared>>,
}

impl RewindObserver for GoldenCheck {
    fn rewound(&mut self, to: &Rewound) -> Result<(), Fatal> {
        let verdict = match self.want.get(&to.height) {
            Some(g) => compare_rewound(to, g),
            None => Err(format!("reverse leg: a rewind to {}, which has no golden", to.height)),
        };
        let mut seen = self.seen.lock().map_err(|_| Fatal::new("l2: the observer's lock"))?;
        match verdict {
            Ok(()) => {
                seen.count += 1;
                seen.muhash += u64::from(to.muhash.is_some());
                Ok(())
            }
            Err(text) => {
                seen.mismatch.get_or_insert_with(|| text.clone());
                Err(Fatal::new(text))
            }
        }
    }
}

/// The reverse leg's log beside `out`: `x.jsonl` gives `x.reverse.jsonl`.
pub fn reverse_path(out: &Path) -> PathBuf {
    out.with_extension("reverse.jsonl")
}

/// Runs `source` under `opts`, writing the forward log to `out` and the reverse leg's to
/// [`reverse_path`] (new files), and the manifest to `manifest` (a new file). `work` is a new
/// directory for the block and undo files, which the caller removes. An output that exists is
/// refused before V starts. A failed run removes the files it created, and only those: a file
/// another writer made at an output's path before the run created it there is left.
pub fn run(
    source: Source<'_>,
    opts: &Options,
    out: &Path,
    manifest: &Path,
    work: &Path,
) -> Result<L2> {
    let reverse = reverse_path(out);
    let snapshot = opts.save_utxo.as_deref().map(|p| [p.to_owned(), sidecar_path(p)]);
    let outputs = [Some(out), opts.reverse_to.map(|_| reverse.as_path()), Some(manifest)]
        .into_iter()
        .flatten()
        .chain(snapshot.iter().flatten().map(PathBuf::as_path));
    for path in outputs {
        match std::fs::symlink_metadata(path) {
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Ok(_) => bail!("{} exists: the run writes only new files", path.display()),
            Err(e) => bail!("checking {}: {e}", path.display()),
        }
    }
    let created = Created::default();
    run_inner(source, opts, out, &reverse, manifest, work, &created).map_err(|mut e| {
        for path in created.0.into_inner().unwrap_or_else(PoisonError::into_inner) {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(rm) if rm.kind() == ErrorKind::NotFound => {}
                Err(rm) => {
                    e = anyhow!("{e:#}; {} is left: removing it failed: {rm}", path.display());
                }
            }
        }
        e
    })
}

/// The files a run has created, each recorded once its `create_new` succeeds: what a failed run
/// removes (CH3a D165).
#[derive(Default)]
struct Created(Mutex<Vec<PathBuf>>);

impl Created {
    /// Creates `path`, a new file, and records it.
    fn file(&self, path: &Path) -> Result<File> {
        let file = create_new(path)?;
        self.0.lock().unwrap_or_else(PoisonError::into_inner).push(path.to_owned());
        Ok(file)
    }
}

/// The run's input, set up.
struct Prepared<'a> {
    leg: Leg,
    /// The window's end.
    to: u32,
    blocks: Blocks<'a>,
    /// The manifest's `input`.
    input: Json,
}

enum Blocks<'a> {
    Dir(&'a BlockDir),
    Node(NodeBlocks),
}

/// A file's sha256, of its bytes as stored.
fn raw_sha256(path: &Path) -> Result<String> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    Sha256Reader::new(file).finish().with_context(|| format!("reading {}", path.display()))
}

/// The header file `opts` names, if any: a blocks directory's, with assumevalid on. Its chain must
/// hold the window `0..=to` of the directory's best chain and reach its tip: Core's index holds the
/// stored blocks' headers too, so its best header is no lower. Above the window only heights are
/// compared, as the directory's best chain is its highest, maybe not Core's.
fn header_file(opts: &Options, source: &Source<'_>) -> Result<Option<HeaderFile>> {
    let Some(path) = &opts.headers else {
        return Ok(None);
    };
    let name = path.display();
    let Source::Dir {
        dir,
        params,
        to,
        ..
    } = source
    else {
        bail!("{name}: the node form preloads no headers");
    };
    ensure!(opts.assume_valid, "{name}: a header file is preloaded: assumevalid must be on");
    let to = *to;
    ensure!(to <= dir.tip(), "the window ends at {to}, past the best chain's tip {}", dir.tip());
    let file = HeaderFile::open(path, genesis_block(params).hash())?;
    ensure!(
        file.tip() >= dir.tip(),
        "{name}: the headers end at {}, below the blocks directory's tip {}",
        file.tip(),
        dir.tip()
    );
    if let Some(h) = (0..=to).find(|&h| file.hash(h) != dir.hash(h)) {
        bail!(
            "{name}: height {h}: the file has {}, the blocks directory {}",
            file.hash(h).expect("at most the file's tip"),
            dir.hash(h).expect("at most the window's end")
        );
    }
    Ok(Some(file))
}

fn prepare<'a>(source: Source<'a>, opts: &Options) -> Result<Prepared<'a>> {
    match source {
        Source::Dir {
            dir,
            params,
            chain,
            to,
        } => {
            ensure!(
                to <= dir.tip(),
                "the window ends at {to}, past the best chain's tip {}",
                dir.tip()
            );
            let (last, _) = dir.headers(to, to).next().context("the window's last header")??;
            let files = dir
                .files(to)?
                .into_iter()
                .map(|(path, size)| {
                    let sha256 = opts.sha256.then(|| raw_sha256(&path)).transpose()?;
                    Ok(json!({"path": path, "size": size, "sha256": sha256}))
                })
                .collect::<Result<Vec<_>>>()?;
            let input = json!({
                "form": "blocks-dir",
                "chain": chain.as_str(),
                "blocks_dir": dir.dir(),
                "best_chain_tip": dir.tip(),
                "files": files,
            });
            let leg = Leg {
                header: header(chain, format!("l2-{}", chain.as_str())),
                params,
                helper: Box::new(IbdHelper),
                miss: MissSlot::new(),
                assume_valid: None,
                start: i64::from(last.time()),
                clock: Box::new(|_| None),
                want: None,
            };
            Ok(Prepared {
                leg,
                to,
                blocks: Blocks::Dir(dir),
                input,
            })
        }
        Source::Node {
            node,
            policy,
        } => {
            ensure!(!opts.assume_valid, "{}: the node form preloads no headers", node.label());
            let (leg, blocks) = node_leg(node, policy)?;
            let shape = &blocks.shape;
            let to = u32::try_from(shape.blocks - 1).context("the node's tip height")?;
            let path = node.blocks();
            let size =
                std::fs::metadata(&path).with_context(|| format!("stat {}", path.display()))?.len();
            let sha256 = opts.sha256.then(|| raw_sha256(&path)).transpose()?;
            // The pre-pass hashed the decompressed stream against `extract.json`.
            let info = json!({
                "form": "node",
                "node": node.label(),
                "files": [{
                    "path": path,
                    "size": size,
                    "sha256": sha256,
                    "decompressed_sha256": shape.sha256,
                }],
            });
            Ok(Prepared {
                leg,
                to,
                blocks: Blocks::Node(blocks),
                input: info,
            })
        }
    }
}

/// The assumevalid and `-minimumchainwork` choices (CH3a D153), each with its reason.
struct Choice {
    assume_valid: Option<BlockHash>,
    min_chain_work: Option<Work>,
    /// The preloaded tip's time: the mock clock starts no earlier.
    clock: Option<i64>,
    json: Json,
}

impl Choice {
    fn off() -> Choice {
        Choice {
            assume_valid: None,
            min_chain_work: None,
            clock: None,
            json: json!({
                "assumevalid": {"mode": "off", "hash": null, "reason": "-assumevalid=0"},
                "minimum_chain_work": {
                    "value": null,
                    "reason": "the chain's default: assumevalid is off, nothing is preloaded",
                },
            }),
        }
    }
}

/// The header chain the preload sends, to its tip (CH3a D153, D162).
#[derive(Clone, Copy)]
enum Preload<'a> {
    /// The blocks directory's best chain.
    Dir(&'a BlockDir),
    /// A header file's chain, which holds the window's blocks.
    File(&'a HeaderFile),
}

impl<'a> Preload<'a> {
    fn tip(self) -> u32 {
        match self {
            Preload::Dir(dir) => dir.tip(),
            Preload::File(file) => file.tip(),
        }
    }

    fn hash(self, h: u32) -> Option<BlockHash> {
        match self {
            Preload::Dir(dir) => dir.hash(h),
            Preload::File(file) => file.hash(h),
        }
    }

    fn headers(
        self,
        from: u32,
        to: u32,
    ) -> Box<dyn Iterator<Item = Result<(dash_consensus::primitives::Header, BlockHash)>> + 'a>
    {
        match self {
            Preload::Dir(dir) => Box::new(dir.headers(from, to)),
            Preload::File(file) => Box::new(file.headers(from, to).map(Ok)),
        }
    }

    /// The manifest's `preload.source`.
    fn to_json(self) -> Json {
        match self {
            Preload::Dir(dir) => json!({"form": "blocks-dir", "tip": dir.tip()}),
            Preload::File(file) => json!({
                "form": "header-file",
                "path": file.path(),
                "sha256": file.sha256(),
                "tip": file.tip(),
            }),
        }
    }
}

/// assumevalid is `defaultAssumeValid` when `chain` holds it, else its tip; `-minimumchainwork`
/// is the default when the tip's work meets it, else the tip's work. A header file must hold
/// `defaultAssumeValid` unless it is null, and its tip must meet the default (CH3a D162).
fn choose(p: &ChainParams, chain: Preload<'_>) -> Result<Choice> {
    let tip = chain.tip();
    let mut work = Work::ZERO;
    let mut time = 0;
    for header in chain.headers(0, tip) {
        let (header, _) = header?;
        work += block_proof(CompactTarget::new(header.bits()));
        time = header.time();
    }
    let tip_hash = chain.hash(tip).context("the preloaded tip")?;
    let default = BlockHash::from(*p.consensus.default_assume_valid.as_bytes());
    let null = default == BlockHash::from([0; 32]);
    let found = (!null).then(|| (0..=tip).find(|&h| chain.hash(h) == Some(default))).flatten();
    let (assume_valid, height, reason) = match found {
        Some(h) => (default, h, format!("defaultAssumeValid is in the preloaded chain, at {h}")),
        None if null => {
            (tip_hash, tip, "the chain's defaultAssumeValid is null: the preloaded tip".to_owned())
        }
        None => {
            // A header file stands for Core's header chain: without the default there, Core
            // would fail assumevalid's first condition (`validation.cpp:2549-2550`) and check
            // every script.
            if let Preload::File(file) = chain {
                bail!(
                    "{}: defaultAssumeValid {default} is not in the header file's chain",
                    file.path().display()
                );
            }
            let reason = format!(
                "defaultAssumeValid {default} is not in the preloaded chain: the preloaded tip"
            );
            (tip_hash, tip, reason)
        }
    };
    let min = p.consensus.minimum_chain_work;
    let (min_chain_work, work_reason) = if work >= min {
        (None, "the chain's default: the preloaded tip's work meets it")
    } else if let Preload::File(file) = chain {
        // As above: below the minimum, Core fails assumevalid's minimum-work condition
        // (`validation.cpp:2553`) and checks every script.
        bail!(
            "{}: the headers' work {work} is below the chain's minimum chain work {min}",
            file.path().display()
        );
    } else {
        (Some(work), "the preloaded tip's work: the chain's default is above it")
    };
    Ok(Choice {
        assume_valid: Some(assume_valid),
        min_chain_work,
        clock: Some(i64::from(time)),
        json: json!({
            "assumevalid": {
                "mode": "on",
                "hash": assume_valid.to_string(),
                "height": height,
                "default": default.to_string(),
                "reason": reason,
            },
            "minimum_chain_work": {
                "value": min_chain_work.unwrap_or(min).to_string(),
                "default": min.to_string(),
                "preloaded_tip_work": work.to_string(),
                "reason": work_reason,
            },
        }),
    })
}

/// What the preload sent and V accepted.
struct Preloaded {
    /// The headers V accepted: the height of the last accepted header its last reply names, not
    /// a count of what was sent.
    headers: u32,
    batches: usize,
    wall: Duration,
}

/// `chain`'s headers from `from` through `process_headers`, [`MAX_HEADERS_RESULTS`] at a time.
fn preload(rig: &Rig, chain: Preload<'_>, from: u32) -> Result<Preloaded> {
    let start = Instant::now();
    let mut headers = chain.headers(from, chain.tip()).peekable();
    let (mut sent, mut accepted, mut batches) = (from as usize - 1, 0, 0);
    while headers.peek().is_some() {
        let mut batch = HeaderBatch {
            headers: Vec::with_capacity(MAX_HEADERS_RESULTS),
            hashes: Vec::with_capacity(MAX_HEADERS_RESULTS),
        };
        for header in headers.by_ref().take(MAX_HEADERS_RESULTS) {
            let (header, hash) = header?;
            batch.headers.push(header);
            batch.hashes.push(hash);
        }
        let (first, n) = (sent + 1, batch.headers.len());
        let last = *batch.hashes.last().expect("a batch holds a header");
        let r = rig
            .v
            .process_headers(batch, None)
            .recv()
            .map_err(|e| anyhow!("the headers from {first}: {e:?}"))?;
        let reader = rig.v.reader();
        accepted = r.last_accepted.and_then(|id| reader.entry(id)).map_or(0, |e| e.height);
        batch_accepted(first, &last, r.stop.as_ref(), r.last_accepted, reader.lookup(&last))?;
        sent += n;
        batches += 1;
    }
    Ok(Preloaded {
        headers: accepted,
        batches,
        wall: start.elapsed(),
    })
}

/// V's reply refuses the batch of headers from `first` to `last` if the batch stopped, or if its
/// last accepted header (Core's `pindexLast`, `validation.cpp:4742-4744`) is not `want`, `last`'s
/// entry.
fn batch_accepted(
    first: usize,
    last: &BlockHash,
    stop: Option<&HeaderStop>,
    last_accepted: Option<BlockId>,
    want: Option<BlockId>,
) -> Result<()> {
    ensure!(stop.is_none(), "the headers from {first} stopped: {stop:?}");
    ensure!(
        want.is_some() && last_accepted == want,
        "the headers from {first}: the last accepted is {last_accepted:?}, not their last, {last}"
    );
    Ok(())
}

/// CPU seconds, user and system.
#[derive(Clone, Copy, Debug, Default)]
struct Cpu {
    user: f64,
    sys: f64,
}

impl Cpu {
    fn to_json(self) -> Json {
        json!({"user_s": self.user, "sys_s": self.sys})
    }
}

/// `getrusage(RUSAGE_SELF)`: the process's CPU.
fn process_cpu() -> Cpu {
    // SAFETY: `getrusage` writes the struct it is given and reads nothing else.
    let mut usage = unsafe { std::mem::zeroed::<libc::rusage>() };
    // SAFETY: as above; RUSAGE_SELF is a valid `who`.
    let ok = unsafe { libc::getrusage(libc::RUSAGE_SELF, &raw mut usage) } == 0;
    let secs = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
    if ok {
        Cpu {
            user: secs(usage.ru_utime),
            sys: secs(usage.ru_stime),
        }
    } else {
        Cpu::default()
    }
}

/// The CPU of this process's threads whose name `wanted` accepts, from `/proc/self/task`; `None`
/// when there is none, or it cannot be read.
fn threads_cpu(wanted: impl Fn(&str) -> bool) -> Option<Cpu> {
    // SAFETY: `sysconf` reads a constant.
    let tick = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64;
    let mut cpu = None::<Cpu>;
    for task in std::fs::read_dir("/proc/self/task").ok()? {
        let task = task.ok()?.path();
        let Ok(comm) = std::fs::read_to_string(task.join("comm")) else {
            continue;
        };
        if !wanted(comm.trim_end()) {
            continue;
        }
        let Ok(stat) = std::fs::read_to_string(task.join("stat")) else {
            continue;
        };
        // After the name's `)`: field 3 (`state`) on, so `utime` (14) and `stime` (15) are the
        // 12th and 13th.
        let fields: Vec<&str> = stat[stat.rfind(')')? + 1..].split_whitespace().collect();
        let ticks = |i: usize| fields.get(i).and_then(|f| f.parse::<f64>().ok());
        let c = cpu.get_or_insert_default();
        c.user += ticks(11)? / tick;
        c.sys += ticks(12)? / tick;
    }
    cpu
}

/// V's thread, and the script-check threads, as `CheckQueue` names them.
fn v_cpu() -> Json {
    let v = threads_cpu(|n| n == "dashd-v");
    let scripts = threads_cpu(|n| n.starts_with("d-scriptch."));
    json!({
        "v_thread": v.map(Cpu::to_json),
        "script_threads": scripts.map(Cpu::to_json),
    })
}

/// `/proc/loadavg`'s three averages.
fn loadavg() -> Json {
    match std::fs::read_to_string("/proc/loadavg") {
        Ok(s) => json!(s.split_whitespace().take(3).collect::<Vec<_>>().join(" ")),
        Err(_) => Json::Null,
    }
}

/// `/proc/cpuinfo`'s first `model name`.
fn cpu_model() -> Json {
    let info = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let model = info.lines().find_map(|l| {
        let (key, value) = l.split_once(':')?;
        (key.trim() == "model name").then(|| value.trim().to_owned())
    });
    json!(model)
}

/// The online processors, `GetNumCores()`'s answer: rdashd's `get_num_cores` on unix, 0 when
/// `sysconf` fails as there. A copy: this crate does not link `dashd-node`, and `dashd-chainstate`
/// denies unsafe code and links `libc` only for its tests.
fn nproc() -> usize {
    // SAFETY: `sysconf` reads a constant.
    let n = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
    usize::try_from(n).unwrap_or(0)
}

/// The UTC date and time, ISO 8601.
fn date() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (days, rest) = (secs / 86_400, secs % 86_400);
    // Days since 1970-01-01 to a civil date (Howard Hinnant's `civil_from_days`).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 {
        mp + 3
    } else {
        mp - 9
    };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3_600,
        rest / 60 % 60,
        rest % 60
    )
}

/// The source tree's git head when the run starts, and whether tracked files differ from it: the
/// build's only when the binary runs from that tree's `target/`. [`exe`] names the binary.
fn git() -> Json {
    let dir = env!("CARGO_MANIFEST_DIR");
    let run = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
    };
    let head = run(&["rev-parse", "HEAD"]);
    let dirty = run(&["status", "--porcelain", "--untracked-files=no"]).map(|s| !s.is_empty());
    json!({"head": head, "dirty": dirty})
}

/// The running executable's path and sha256: the binary itself, wherever it was copied (CH3a
/// D165).
fn exe() -> Result<Json> {
    let path = std::env::current_exe().context("the running executable's path")?;
    Ok(json!({"path": path, "sha256": raw_sha256(&path)?}))
}

/// Each coin in the cache, which must hold them all, as `f` sees it.
fn each_coin(
    coins: &Coins,
    mut f: impl FnMut(&OutPointKey, CoinRef<'_>) -> Result<()>,
) -> Result<()> {
    let cache = coins.lock().map_err(|_| anyhow!("the coins lock is poisoned"))?;
    ensure!(
        cache.durable().is_none() && cache.frozen().is_none(),
        "the coins were flushed: the cache alone does not hold them"
    );
    for (key, slot) in cache.active().iter() {
        // A tombstone: a spent coin.
        if slot.coin.is_empty() {
            continue;
        }
        let mut bytes = slot.coin;
        let coin =
            CoinRef::decode(&mut bytes).map_err(|e| anyhow!("coin {:?}: {e:?}", key.outpoint()))?;
        f(&key, coin)?;
    }
    Ok(())
}

/// A coins scan: the set's MuHash state and its values. The state is finalized in place for
/// `muhash`, which divides it down to the quotient: later results are unchanged.
fn scan_set(coins: &Coins) -> Result<(MuHash3072, EndCheck)> {
    let mut muhash = MuHash3072::new();
    let (mut txouts, mut amount) = (0u64, 0i64);
    each_coin(coins, |key, coin| {
        muhash.insert_coin(&key.outpoint(), &coin);
        txouts += 1;
        amount = amount.checked_add(coin.value()).context("the coins' amount overflows")?;
        Ok(())
    })?;
    let end = EndCheck {
        muhash: Hash256::from_wire_bytes(muhash.finalize()),
        txouts,
        amount,
    };
    Ok((muhash, end))
}

/// A coins scan: every coin in the cache, which must hold them all.
fn scan(coins: &Coins) -> Result<EndCheck> {
    Ok(scan_set(coins)?.1)
}

/// `what`, a scan at `l`, against golden(`l`).
fn check_coins(what: &str, end: &EndCheck, l: u32, g: &Golden) -> Result<()> {
    let differ = |key: &str, have: String, want: String| {
        bail!("{what} at {l} gives {key} {have}, golden({l}) has {want}")
    };
    if end.muhash != g.muhash {
        return differ("muhash", end.muhash.to_hex(), g.muhash.to_hex());
    }
    if end.txouts != g.txouts {
        return differ("txouts", end.txouts.to_string(), g.txouts.to_string());
    }
    if end.amount != g.amount {
        return differ("amount", end.amount.to_string(), g.amount.to_string());
    }
    Ok(())
}

/// The start check (CH3a D166; DEC-348 (2)): the coins a pruned start loaded at `base` give
/// golden(`base`)'s `muhash`, `txouts` and `amount`, and are the snapshot's `count`. The scan's
/// MuHash state, which the log is seeded with, and its values.
fn check_start(
    coins: &Coins,
    base: u32,
    g: &Golden,
    count: usize,
) -> Result<(MuHash3072, EndCheck)> {
    let (muhash, end) = scan_set(coins)?;
    check_coins("start check: the snapshot's coins", &end, base, g)?;
    ensure!(
        end.txouts == count as u64,
        "start check: the cache holds {} coins, the snapshot {count}",
        end.txouts
    );
    Ok((muhash, end))
}

/// The accumulator a pruned start's log is seeded with (CH3a D165): the base's MuHash state, the
/// scan's `txouts` and `amount`, golden(B)'s `unspendable`, and `total_subsidy` equal to it, so
/// that the index's balance is 0 as Core's is after every block
/// (`index/coinstatsindex.cpp:208-210`); the other totals 0, which `bi`'s differences cancel.
fn seed(muhash: MuHash3072, end: &EndCheck, g: &Golden) -> CoinStatsAccumulator {
    let totals = CoinStatsTotals {
        transaction_output_count: end.txouts,
        total_amount: end.amount,
        total_unspendable_amount: g.unspendable,
        total_subsidy: g.unspendable,
        ..CoinStatsTotals::default()
    };
    CoinStatsAccumulator::from_parts(muhash, totals)
}

/// Every coin in the cache, owned.
fn coins_of(coins: &Coins) -> Result<Vec<(OutPointKey, Coin)>> {
    let mut out = Vec::new();
    each_coin(coins, |key, coin| {
        out.push((*key, coin.to_coin()));
        Ok(())
    })?;
    Ok(out)
}

/// Where `--save-utxo` writes, and what the coins must equal there.
struct Save<'a> {
    path: &'a Path,
    golden: &'a Golden,
    /// The sidecar's `chain` and `run`.
    chain: Chain,
    run: Json,
}

/// Writes the coins at `height`, block `hash`, to `s.path` and its sidecar, once a scan of them
/// gives golden(`height`)'s three values (CH3a D165). The manifest's `save_utxo`.
fn save_utxo(
    coins: &Coins,
    s: &Save<'_>,
    height: u32,
    hash: BlockHash,
    created: &Created,
) -> Result<Json> {
    let end = scan(coins)?;
    check_coins("save check: the coins scan", &end, height, s.golden)?;
    let set = coins_of(coins)?;
    let count = set.len();
    let sha256 = utxo::write(created.file(s.path)?, s.path, hash, set)?;
    let sidecar = json!({
        "format": "dumptxoutset, Core v24.0.0-rc.3 (rpc/blockchain.cpp:3147-3203)",
        "chain": s.chain.as_str(),
        "base": {"height": height, "hash": hash.to_string()},
        "coins": count,
        "sha256": sha256,
        "scan": {"muhash": end.muhash.to_hex(), "txouts": end.txouts, "amount": end.amount},
        "run": s.run,
    });
    let side = sidecar_path(s.path);
    let mut file = created.file(&side)?;
    let mut bytes = serde_json::to_vec_pretty(&sidecar)?;
    bytes.push(b'\n');
    file.write_all(&bytes).with_context(|| format!("writing {}", side.display()))?;
    Ok(json!({"path": s.path, "sidecar": side, "coins": count, "sha256": sha256}))
}

/// What V did while it ran.
struct Driven {
    preload: Option<Preloaded>,
    tip: (u32, Hash256),
    forward: Tally,
    v_time: Duration,
    forward_cpu: Json,
    reverse: Option<(Tally, Duration)>,
    end_cpu: Json,
    saved: Option<Json>,
}

/// The parts of the run V's lifetime spans.
struct Drive<'a> {
    blocks: Blocks<'a>,
    /// The block V's chain starts at: genesis, or a pruned start's base.
    first: FeedStart,
    to: u32,
    preload: Option<Preload<'a>>,
    clock: &'a dyn Fn(&dash_consensus::Block) -> Option<i64>,
    miss: &'a MissSlot,
    tally: &'a Mutex<Tally>,
    switch: &'a Switch,
    reverse: Option<(u32, &'a Path, Header)>,
    save: Option<Save<'a>>,
    created: &'a Created,
}

fn take(tally: &Mutex<Tally>) -> Result<Tally> {
    Ok(std::mem::take(&mut *tally.lock().map_err(|_| anyhow!("the tally lock is poisoned"))?))
}

/// The preload, the forward run, the snapshot and the reverse leg, with V running.
fn drive(rig: &Rig, d: Drive<'_>) -> Result<Driven> {
    // Core's init connects genesis before any peer can send a header: it waits for the genesis
    // tip before it starts the connection manager. `feed`'s own init ABC then has nothing to do.
    // Core: init.cpp:2563-2573, :2755
    if d.preload.is_some() {
        rig.v.activate_best_chain().recv().map_err(|e| anyhow!("init's ABC: {e:?}"))?;
    }
    let preload = d.preload.map(|chain| preload(rig, chain, d.first.height + 1)).transpose()?;

    let (mut first, mut last) = (None, None);
    let on = |step| {
        let now = Instant::now();
        match step {
            Step::Send => {
                first.get_or_insert(now);
            }
            Step::Reply => last = Some(now),
        }
    };
    let (fed, node_input) = match d.blocks {
        Blocks::Dir(dir) => {
            let fed = std::thread::scope(|scope| {
                let (tx, rx) = sync_channel(READ_AHEAD);
                let reader = scope.spawn(move || {
                    for block in dir.blocks_from(d.first.height, d.to) {
                        if tx.send(block).is_err() {
                            break;
                        }
                    }
                });
                let mut blocks = rx.into_iter();
                let fed = feed(rig, &mut blocks, d.first, d.clock, d.miss, on);
                drop(blocks);
                reader.join().map_err(|_| anyhow!("the block reader panicked")).map(|()| fed)
            })?;
            (fed, None)
        }
        Blocks::Node(mut blocks) => {
            (feed(rig, &mut blocks.frames, d.first, d.clock, d.miss, on), Some(blocks))
        }
    };
    let v_time = match (first, last) {
        (Some(first), Some(last)) => last - first,
        _ => Duration::ZERO,
    };
    let forward_cpu = v_cpu();
    let handed = fed.handed?;
    if let Some(blocks) = node_input {
        blocks.finish(fed.read)?;
    }
    let tip = check_tip(&rig.v.view(), fed.last, d.first.height as usize + handed)?;
    ensure!(tip.0 == d.to, "V's tip is at {}, not the window's end {}", tip.0, d.to);
    let forward = take(d.tally)?;
    // V is idle between commands: the coins are at the window's end.
    let saved =
        d.save.map(|s| save_utxo(&rig.coins, &s, tip.0, fed.last, d.created)).transpose()?;

    let reverse = match d.reverse {
        None => None,
        Some((l, path, header)) => {
            let mut file = d.created.file(path)?;
            file.write_all(&Record::Header(header).to_line())
                .and_then(|()| file.flush())
                .with_context(|| format!("writing {}", path.display()))?;
            *d.switch.file()? = file;
            let start = Instant::now();
            if l < tip.0 {
                let reader = rig.v.reader();
                let tip_id = reader.lookup(&fed.last).context("V's tip is not indexed")?;
                let id = reader.ancestor(tip_id, l + 1).context("block L+1 is not on V's chain")?;
                let hash = reader.entry(id).context("block L+1 has no entry")?.hash;
                rig.v
                    .invalidate_block(hash)
                    .recv()
                    .map_err(|e| anyhow!("invalidate_block({}): {e:?}", l + 1))?
                    .map_err(|_| anyhow!("invalidate_block({}): no block {hash}", l + 1))?;
            }
            let wall = start.elapsed();
            let at = &rig.v.view().snap.tip;
            ensure!(
                at.tip_height == l,
                "after the reverse leg V's tip is at {}, not {l}",
                at.tip_height
            );
            Some((take(d.tally)?, wall))
        }
    };
    Ok(Driven {
        preload,
        tip,
        forward,
        v_time,
        forward_cpu,
        reverse,
        end_cpu: v_cpu(),
        saved,
    })
}

/// A pruned start's snapshot as its sidecar names it (CH3a D165).
struct Sidecar {
    path: PathBuf,
    /// The base's height and hash.
    height: u32,
    hash: Hash256,
    coins: usize,
    sha256: String,
}

/// The sidecar of the snapshot at `path`, which must be of `chain`.
fn read_sidecar(path: &Path, chain: Chain) -> Result<Sidecar> {
    let side = sidecar_path(path);
    let name = side.display();
    let bytes = std::fs::read(&side).with_context(|| format!("reading {name}"))?;
    let json: Json = serde_json::from_slice(&bytes).with_context(|| format!("{name}: not JSON"))?;
    let field = |ptr: &str| json.pointer(ptr).with_context(|| format!("{name}: no {ptr}"));
    let text =
        |ptr: &str| field(ptr)?.as_str().with_context(|| format!("{name}: {ptr} is not text"));
    let count =
        |ptr: &str| field(ptr)?.as_u64().with_context(|| format!("{name}: {ptr} is not a count"));
    let have = text("/chain")?;
    ensure!(have == chain.as_str(), "{name}: a {have} snapshot, not {}", chain.as_str());
    Ok(Sidecar {
        height: u32::try_from(count("/base/height")?).context("the base's height")?,
        hash: Hash256::from_hex(text("/base/hash")?.as_bytes())
            .with_context(|| format!("{name}: /base/hash is not a hash"))?,
        coins: usize::try_from(count("/coins")?).context("the coin count")?,
        sha256: text("/sha256")?.to_owned(),
        path: side,
    })
}

/// The pruned start `side` names: its snapshot, which must be the sidecar's and hold the base's
/// coins, and the index of `dir`'s chain to the base, whose block there must be the sidecar's
/// and golden(B)'s. Each record has golden's `nTx` and no data (CH3a D165, D166). The manifest's
/// `start`, less its check.
fn pruned_start(
    snapshot: &Path,
    side: &Sidecar,
    dir: &BlockDir,
    want: &BTreeMap<u32, Golden>,
    ntx: &[u32],
) -> Result<(Pruned, Json)> {
    let (b, name) = (side.height, side.path.display());
    let g = &want[&b];
    ensure!(
        side.hash == g.b,
        "{name}: the base {} at {b} is not golden({b})'s block {}",
        side.hash.to_hex(),
        g.b.to_hex()
    );
    let base =
        dir.hash(b).with_context(|| format!("{name}: the base {b} is past the chain's tip"))?;
    ensure!(
        display(base) == side.hash,
        "{name}: the base {} at {b} is not the header chain's block {base}",
        side.hash.to_hex()
    );
    let set = utxo::read(snapshot, b)?;
    let file = snapshot.display();
    ensure!(
        display(set.base) == side.hash,
        "{file}: its base {} is not the sidecar's {}",
        set.base,
        side.hash.to_hex()
    );
    ensure!(
        set.sha256 == side.sha256,
        "{file}: its sha256 {} is not the sidecar's {}",
        set.sha256,
        side.sha256
    );
    ensure!(
        set.coins.len() == side.coins,
        "{file}: {} coins, the sidecar says {}",
        set.coins.len(),
        side.coins
    );
    let records = dir
        .headers(0, b)
        .zip(0..)
        .map(|(header, h)| {
            let (header, hash) = header?;
            // Genesis stays VALID_TRANSACTIONS: its ConnectBlock returns before the raise.
            // Core: validation.cpp:2536-2540, :2899-2900
            let valid = if h == 0 {
                Status::VALID_TRANSACTIONS
            } else {
                Status::VALID_SCRIPTS
            };
            let record = BlockIndexRecord {
                height: h,
                status: Status(valid),
                n_tx: ntx[h as usize],
                file: None,
                data_pos: None,
                undo_pos: None,
                header: *header.raw(),
            };
            Ok((hash, record.to_bytes()))
        })
        .collect::<Result<Vec<_>>>()?;
    let json = json!({
        "utxo": snapshot,
        "sha256": set.sha256,
        "sidecar": side.path,
        "sidecar_sha256": raw_sha256(&side.path)?,
        "base": {"height": b, "hash": base.to_string()},
        "coins": set.coins.len(),
        "index_records": records.len(),
        "have_pruned": true,
    });
    let pruned = Pruned {
        records,
        base,
        coins: set.coins,
    };
    Ok((pruned, json))
}

/// The forward log's `C` at each height of `at` against the golden (CH3a D165): from a pruned
/// base the log finalizes its MuHash at those heights only, the window's end and the reverse
/// leg's end when the forward run passes it. The manifest's `forward_golden`.
fn check_forward(out: &Path, at: &[u32], want: &BTreeMap<u32, Golden>) -> Result<Json> {
    let checked = at.iter().map(|&h| {
        let ours = golden_range(out, h, h).context("the forward log")?.remove(&h).expect("read");
        let g = &want[&h];
        let keys = [
            ("the block", ours.b.to_hex(), g.b.to_hex()),
            ("muhash", ours.muhash.to_hex(), g.muhash.to_hex()),
            ("txouts", ours.txouts.to_string(), g.txouts.to_string()),
            ("amount", ours.amount.to_string(), g.amount.to_string()),
            ("unspendable", ours.unspendable.to_string(), g.unspendable.to_string()),
        ];
        if let Some((key, have, want)) = keys.iter().find(|(_, have, want)| have != want) {
            bail!("forward log: at {h}, {key} is {have}, golden({h}) has {want}");
        }
        Ok(json!({
            "height": h,
            "muhash": ours.muhash.to_hex(),
            "txouts": ours.txouts,
            "amount": ours.amount,
            "unspendable": ours.unspendable,
            "equal": true,
        }))
    });
    Ok(Json::Array(checked.collect::<Result<_>>()?))
}

fn run_inner(
    source: Source<'_>,
    opts: &Options,
    out: &Path,
    reverse_out: &Path,
    manifest: &Path,
    work: &Path,
    created: &Created,
) -> Result<L2> {
    let started = Instant::now();
    let (load_start, date, exe, git) = (loadavg(), date(), exe()?, git());
    // What the process spent before the run: the CLI's index of a blocks directory (and the
    // binary's hash above).
    let cpu_start = process_cpu();
    let file = header_file(opts, &source)?;
    // A header file means a blocks directory with assumevalid on (checked above).
    let preload_from = match (&file, &source) {
        (Some(file), _) => Some(Preload::File(file)),
        (
            None,
            Source::Dir {
                dir,
                ..
            },
        ) if opts.assume_valid => Some(Preload::Dir(dir)),
        _ => None,
    };
    let Prepared {
        mut leg,
        to,
        blocks,
        input,
    } = prepare(source, opts)?;
    let p = leg.params.clone();
    let chain = leg.header.chain;

    // A pruned start's sidecar, then the window around its base.
    let side = match (&opts.start_utxo, &blocks) {
        (None, _) => None,
        (Some(path), Blocks::Dir(_)) => Some(read_sidecar(path, chain)?),
        (Some(path), Blocks::Node(_)) => {
            bail!("{}: the node form has no pruned start", path.display())
        }
    };
    let base = side.as_ref().map(|s| s.height);
    if let Some(b) = base {
        ensure!(b < to, "--start-utxo: the window ends at {to}, not past the snapshot's base {b}");
    }
    if let Some(l) = opts.reverse_to {
        ensure!(l <= to, "--reverse-to {l} is above the window's end {to}");
        if let Some(b) = base {
            ensure!(
                l >= b,
                "--reverse-to {l} is below the snapshot's base {b}: its blocks are pruned"
            );
        }
    }

    // The golden, read once before V starts: the reverse leg's L..=N, a pruned start's B and N
    // with each nTx to B, and N for a snapshot.
    let want = if opts.reverse_to.is_some() || base.is_some() || opts.save_utxo.is_some() {
        let path = opts
            .golden
            .as_deref()
            .context("--reverse-to, --start-utxo and --save-utxo need a golden")?;
        let from = [opts.reverse_to, base, Some(to)].into_iter().flatten().min().unwrap_or(to);
        let (want, ntx) = read_golden(path, from, to, base)?;
        Some((Arc::new(want), ntx))
    } else {
        None
    };
    let (first, origin, start_json, coins_count) = match (&side, &blocks, &want, &opts.start_utxo) {
        (Some(side), Blocks::Dir(dir), Some((want, ntx)), Some(path)) => {
            let (pruned, json) = pruned_start(path, side, dir, want, ntx)?;
            let first = FeedStart {
                hash: pruned.base,
                height: side.height,
            };
            let count = pruned.coins.len();
            (first, Start::Pruned(pruned), Some(json), count)
        }
        _ => (FeedStart::genesis(&p), Start::Genesis, None, 0),
    };
    let want = want.map(|(want, _)| want);

    let choice = match preload_from {
        Some(chain) => choose(&p, chain)?,
        None => Choice::off(),
    };
    leg.assume_valid = choice.assume_valid;
    if let Some(t) = choice.clock {
        leg.start = leg.start.max(t);
    }
    let mut config = ChainConfig::node(&p);
    config.min_chain_work = choice.min_chain_work;
    let queue = CheckQueue::new(script_check_threads(opts.par, nproc()))
        .context("starting the script-check threads")?;
    // The manifest names the threads the queue runs.
    let threads = queue.threads();
    let settings = Settings {
        config,
        check_block_index: p.default_consistency_checks,
        queue,
        coins_share: opts.coins_cache,
        start: origin,
    };

    // The logs: the forward one now, the reverse leg's when it starts.
    let switch = Switch(Arc::new(Mutex::new(created.file(out)?)));
    let reverse_header = Header {
        run: [leg.header.run.as_slice(), b"-reverse"].concat(),
        ..leg.header.clone()
    };
    let tally = Arc::new(Mutex::new(Tally::default()));
    let compared = Arc::new(Mutex::new(Compared::default()));
    // From a pruned base the log finalizes its MuHash at the window's end and the reverse leg's
    // only: at every height it would cost R11's suffix check most of its time (CH3a D165).
    let muhash_heights: Option<BTreeSet<u32>> =
        base.map(|_| [Some(to), opts.reverse_to].into_iter().flatten().collect());
    let muhash_at = muhash_heights.clone().map_or(MuHashAt::Every, MuHashAt::Only);
    let mut start_check = None;
    let header = leg.header.clone();
    let sink: MakeSink<'_> = Box::new(|at, coins| {
        let log = match at {
            None => DigestLog::new(switch.clone(), header),
            Some(at) => {
                let g = &want.as_ref().context("a pruned start without a golden")?[&at.height];
                let (muhash, end) = check_start(coins, at.height, g, coins_count)?;
                let stats = seed(muhash, &end, g);
                start_check = Some(end);
                DigestLog::seeded(switch.clone(), header, stats, at.id)
            }
        };
        let mut log = log.map_err(|e| anyhow!("{}", e.0))?.with_muhash_at(muhash_at);
        if let (Some(want), Some(_)) = (&want, opts.reverse_to) {
            log = log.with_observer(Box::new(GoldenCheck {
                want: want.clone(),
                seen: compared.clone(),
            }));
        }
        Ok(Box::new(Tallied {
            log,
            tally: tally.clone(),
        }))
    });

    let Leg {
        helper,
        miss,
        assume_valid,
        start,
        clock,
        ..
    } = leg;
    let rig = Rig::start(&p, helper, assume_valid, start, sink, settings, work)?;
    let coins = rig.coins.clone();
    let save = match (&opts.save_utxo, &want) {
        (Some(path), Some(want)) => Some(Save {
            path,
            golden: &want[&to],
            chain,
            run: json!({
                "tool": "dash-conformance digest l2",
                "version": env!("CARGO_PKG_VERSION"),
                "git": git,
                "exe": exe,
                "date": date,
                "digest": out,
                "start_sha256": start_json.as_ref().map(|s| s["sha256"].clone()),
            }),
        }),
        _ => None,
    };
    let driven = drive(
        &rig,
        Drive {
            blocks,
            first,
            to,
            preload: preload_from,
            clock: &clock,
            miss: &miss,
            tally: &tally,
            switch: &switch,
            reverse: opts.reverse_to.map(|l| (l, reverse_out, reverse_header)),
            save,
            created,
        },
    );
    let mismatch = compared.lock().map_err(|_| anyhow!("the observer's lock"))?.mismatch.clone();
    let stopped = rig.stop(&miss);
    if let Some(text) = mismatch {
        bail!("{text}");
    }
    stopped?;
    let driven = driven?;

    // From a pruned base, the forward log's MuHash where it was finalized.
    let forward_golden = match (base, &want, &muhash_heights) {
        (Some(b), Some(want), Some(at)) => {
            let at: Vec<u32> = at.iter().copied().filter(|&h| h > b).collect();
            Some(check_forward(out, &at, want)?)
        }
        _ => None,
    };

    // The reverse leg's counts, then the coins themselves.
    let reverse = match (opts.reverse_to, &want, &driven.reverse) {
        (Some(l), Some(want), Some((tally, _))) => {
            let n = u64::from(to - l);
            ensure!(
                tally.disconnects == n && tally.disconnects_ok == n,
                "the reverse leg to {l} made {} disconnects, {} ok, not {n}",
                tally.disconnects,
                tally.disconnects_ok
            );
            ensure!(
                tally.connects == 0 && tally.rejects == 0,
                "the reverse leg connected {} blocks and rejected {}",
                tally.connects,
                tally.rejects
            );
            let seen = compared.lock().map_err(|_| anyhow!("the observer's lock"))?;
            ensure!(seen.count == n, "the observer compared {} rewinds, not {n}", seen.count);
            // From a pruned base only the last rewind, to L, finalizes its MuHash.
            let muhash = if base.is_some() {
                u64::from(n > 0)
            } else {
                n
            };
            ensure!(
                seen.muhash == muhash,
                "the observer compared {} rewinds' muhash, not {muhash}",
                seen.muhash
            );
            let (count, compared_muhash) = (seen.count, seen.muhash);
            drop(seen);
            let end = scan(&coins)?;
            check_coins("end check: the coins scan", &end, l, &want[&l])?;
            Some((
                Reversed {
                    to: l,
                    tally: tally.clone(),
                    compared: count,
                    end,
                },
                compared_muhash,
            ))
        }
        _ => None,
    };
    let start_json = start_json.map(|mut json| {
        let check = start_check.as_ref().expect("a pruned start checks its coins");
        json["check"] = json!({
            "method": "a scan of every coin loaded, before V starts",
            "muhash": check.muhash.to_hex(),
            "txouts": check.txouts,
            "amount": check.amount,
            "golden_height": base,
            "equal": true,
        });
        json["muhash_at"] = json!(muhash_heights);
        json
    });

    let manifest_json = json!({
        "tool": "dash-conformance digest l2",
        "version": env!("CARGO_PKG_VERSION"),
        "git": git,
        "exe": exe,
        "date": date,
        "host": {
            "cpu_model": cpu_model(),
            "nproc": nproc(),
            "loadavg_start": load_start,
            "loadavg_end": loadavg(),
        },
        "input": input,
        "window": {"from": base.map_or(0, |b| b + 1), "to": to, "tip": driven.tip.1.to_hex()},
        "start": start_json,
        "out": {"digest": out, "reverse": reverse.as_ref().map(|_| reverse_out)},
        "choices": choice.json,
        "preload": driven.preload.as_ref().map(|pl| json!({
            "source": preload_from.map(Preload::to_json),
            "headers": pl.headers,
            "batches": pl.batches,
            "batch_size": MAX_HEADERS_RESULTS,
            "wall_s": pl.wall.as_secs_f64(),
        })),
        "par": opts.par,
        "script_check_threads": threads,
        "coins_cache_bytes": opts.coins_cache,
        "check_block_index": p.default_consistency_checks,
        "forward": driven.forward.to_json(),
        "forward_golden": forward_golden,
        "save_utxo": driven.saved,
        "reverse": reverse.as_ref().map(|(r, muhash)| json!({
            "to": r.to,
            "tally": r.tally.to_json(),
            "compared_with_golden": r.compared,
            "compared_muhash": muhash,
            "end_check": {
                "method": "a scan of every coin in the cache",
                "muhash": r.end.muhash.to_hex(),
                "txouts": r.end.txouts,
                "amount": r.end.amount,
                "golden_height": r.to,
                "equal": true,
            },
            "wall_s": driven.reverse.as_ref().map(|(_, w)| w.as_secs_f64()),
        })),
        "timings": {
            "v_time_s": driven.v_time.as_secs_f64(),
            "v_time_definition": "wall time from the first process_block sent to the last reply",
            "total_wall_s": started.elapsed().as_secs_f64(),
            "process_cpu": process_cpu().to_json(),
            "process_cpu_before_run": cpu_start.to_json(),
            "after_forward": driven.forward_cpu,
            "at_end": driven.end_cpu,
        },
    });
    let mut file = created.file(manifest)?;
    let mut bytes = serde_json::to_vec_pretty(&manifest_json)?;
    bytes.push(b'\n');
    file.write_all(&bytes).with_context(|| format!("writing {}", manifest.display()))?;
    Ok(L2 {
        tip: driven.tip,
        forward: driven.forward,
        reverse: reverse.map(|(r, _)| r),
        start: start_check,
        manifest: manifest_json,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn golden() -> Golden {
        Golden {
            b: Hash256::from_hex(&[b'1'; 64]).unwrap(),
            muhash: Hash256::from_hex(&[b'2'; 64]).unwrap(),
            txouts: 7,
            amount: 50,
            unspendable: 3,
        }
    }

    fn rewound(g: &Golden) -> Rewound {
        Rewound {
            height: 12,
            hash: g.b,
            muhash: Some(g.muhash),
            txouts: g.txouts,
            amount: g.amount,
        }
    }

    /// CH3a D152: a corrupted golden value is reported at its height and key, the first that
    /// differs in the leg's order.
    #[test]
    fn a_corrupted_golden_value_is_named_by_height_and_key() {
        let g = golden();
        assert_eq!(compare_rewound(&rewound(&g), &g), Ok(()));
        let corrupt = |f: fn(&mut Golden)| {
            let mut bad = g.clone();
            f(&mut bad);
            compare_rewound(&rewound(&g), &bad).unwrap_err()
        };
        assert_eq!(corrupt(|g| g.txouts = 8), "reverse leg: at 12, txouts is 7, golden(12) has 8");
        assert_eq!(
            corrupt(|g| g.amount = 49),
            "reverse leg: at 12, amount is 50, golden(12) has 49"
        );
        let e = corrupt(|g| g.muhash = Hash256::from_hex(&[b'3'; 64]).unwrap());
        assert!(e.starts_with("reverse leg: at 12, muhash is 2222"), "{e}");
        let e = corrupt(|g| {
            g.b = Hash256::from_hex(&[b'4'; 64]).unwrap();
            g.txouts = 0;
        });
        assert!(e.starts_with("reverse leg: at 12, the block is 1111"), "{e}");
        // CH3a D165: a rewind that finalized no MuHash is compared on the other keys.
        let bare = Rewound {
            muhash: None,
            ..rewound(&g)
        };
        let mut other = g.clone();
        other.muhash = Hash256::from_hex(&[b'3'; 64]).unwrap();
        assert_eq!(compare_rewound(&bare, &other), Ok(()));
        other.amount = 49;
        assert_eq!(
            compare_rewound(&bare, &other).unwrap_err(),
            "reverse leg: at 12, amount is 50, golden(12) has 49"
        );
    }

    /// CH3a D153 (Opus a7-2 r1 N2): a batch V stopped, or whose last accepted header is not its
    /// last, fails the preload, each check alone.
    #[test]
    fn a_batch_v_stopped_or_did_not_accept_to_its_end_fails_the_preload() {
        let last = BlockHash::from_lendian([7; 32]);
        let (end, short) = (Some(BlockId(4_000)), Some(BlockId(2_099)));
        assert!(batch_accepted(2_001, &last, None, end, end).is_ok());
        let stop = HeaderStop::BelowAnchor {
            index: 99,
        };
        let e = batch_accepted(2_001, &last, Some(&stop), end, end).unwrap_err().to_string();
        assert_eq!(e, "the headers from 2001 stopped: Some(BelowAnchor { index: 99 })");
        let e = batch_accepted(2_001, &last, None, short, end).unwrap_err().to_string();
        assert!(e.starts_with("the headers from 2001: the last accepted is Some(BlockId(2099)), "));
        assert!(e.ends_with(&format!("not their last, {last}")), "{e}");
        assert!(batch_accepted(2_001, &last, None, None, None).is_err(), "an unknown last header");
    }

    #[test]
    fn the_tally_counts_runs_of_decisions() {
        let mut t = Tally::default();
        for (h, d) in
            [(0, None), (1, Some(false)), (2, Some(false)), (3, Some(true)), (5, Some(true))]
        {
            t.run(h, d);
        }
        assert_eq!(
            t.runs,
            [(0, 0, None), (1, 2, Some(false)), (3, 3, Some(true)), (5, 5, Some(true))]
        );
    }

    #[test]
    fn dates_and_paths() {
        let d = date();
        assert_eq!((d.len(), &d[4..5], &d[10..11], &d[19..]), (20, "-", "T", "Z"), "{d}");
        assert_eq!(reverse_path(Path::new("/a/b.jsonl")), Path::new("/a/b.reverse.jsonl"));
        assert_eq!(reverse_path(Path::new("b")), Path::new("b.reverse.jsonl"));
    }

    /// CH3a D165: a path is recorded only once the run has created its file, so a failed run never
    /// removes one that another writer made.
    #[test]
    fn a_run_records_only_the_files_it_creates() {
        let dir = tempfile::tempdir().unwrap();
        let (other, new) = (dir.path().join("other"), dir.path().join("new"));
        std::fs::write(&other, "kept").unwrap();
        let created = Created::default();
        assert!(created.file(&other).is_err());
        created.file(&new).unwrap();
        assert_eq!(created.0.into_inner().unwrap(), [new]);
    }
}
