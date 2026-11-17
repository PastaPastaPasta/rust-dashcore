//! The census (O2b §6.5): what the scored inputs reached, and what the generator and the tie
//! model did. `fuzzctl` reads it from the stats file (`NODE_L1_CHAIN_STATS`).
//!
//! An input's census merges into the run's only if the input was not discarded (D30); the
//! discards themselves are counted by site and op kind.
//!
//! A certify run's replays write theirs as phase-bound evidence ([`Writer`], O2b-4 §4.6, D56):
//! by rename, naming the run, and complete only when the process's exit wrote it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::step::ReplyClass;
use super::ties::TieSite;

/// One input's counts.
#[derive(Clone, Debug, Default)]
pub struct Census {
    /// `(op kind, reply class)`.
    pub ops: BTreeMap<(&'static str, &'static str), u64>,
    /// Every reject string reached (BIP22 strings and RPC errors' messages).
    pub rejects: BTreeMap<String, u64>,
    /// The named rows of O2b §6.5 this input reached.
    pub rows: BTreeMap<&'static str, u64>,
    /// Intents the generator dropped (the tie model, an unadmitted spork), by reason.
    pub avoided: BTreeMap<&'static str, u64>,
    /// Restarts deferred behind an extension of the active chain (G-T1).
    pub deferred_restarts: u64,
    /// Replies the generator did not expect, by `op kind: expected`.
    pub unexpected: BTreeMap<String, u64>,
    /// The inventory occurrences reached (O2b-4 §4.2), by a10 inventory key: credited only where
    /// the op's expectation named the site and the reply carried its reason ([`Census::credit`]);
    /// with each wrapper row whose callee that is, and `script:<error>` for a script failure's
    /// error.
    pub inventory: BTreeMap<String, u64>,
    /// Leg B's refused opens whose cleanup flushes differed (O2b-4 §2.3a), by the node's first
    /// unmatched label.
    pub legb_refused_open: BTreeMap<String, u64>,
}

/// The script failure's inline occurrence, whose reply carries the script error.
const CIS_SCRIPT: &str =
    "validation.cpp:CheckInputScripts#2:mandatory-script-verify-flag-failed (%s)";
const CIS_REASON: &str = "mandatory-script-verify-flag-failed (";

/// The wrapper (W) rows a reply proves propagated, by their callee function (a10 inventory
/// `callee`): the evo wrappers' callees are never reached below DIP3.
const WRAPPERS: &[(&str, &str)] = &[
    ("CheckTransaction", "validation.cpp:CheckBlock@W:CheckTransaction"),
    (
        "ContextualCheckTransaction",
        "validation.cpp:ContextualCheckBlock@W:ContextualCheckTransaction",
    ),
    ("Consensus::CheckTxInputs", "validation.cpp:ConnectBlock@W:Consensus::CheckTxInputs"),
    ("CheckInputScripts", "validation.cpp:ConnectBlock@W:CheckInputScripts"),
    ("CheckSpecialTxInner", "specialtxman.cpp:ProcessSpecialTxsInBlock@W:CheckSpecialTxInner"),
];

/// The function an inventory key names (`tx_verify.cpp:Consensus::CheckTxInputs#1:…` →
/// `Consensus::CheckTxInputs`).
fn function_of(key: &str) -> &str {
    let body = key.split_once(':').map_or(key, |(_, b)| b);
    body.split(['#', '@']).next().unwrap_or(body)
}

/// Whether reply `text` carries `reason` whole (OD-L3-155): it is `reason`, or goes on past it
/// with a character outside `[A-Za-z0-9_-]`, which no reason continues with (`bad-prevblk` is
/// not carried by `bad-prevblk-chainlock`, `bad-version(0x00000002)` is by
/// `bad-version(0x00000002), rejected nVersion=0x00000002 block`).
pub(crate) fn carries(text: &str, reason: &str) -> bool {
    let token = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
    text.strip_prefix(reason).is_some_and(|rest| rest.chars().next().is_none_or(|c| !token(c)))
}

/// The row counting every leg-B step: [`Census::legb_step_share`]'s denominator.
pub const LEGB_STEPS: &str = "legb_steps";
/// The row counting leg-B steps with at least one natural policy mirror: the step share's
/// numerator.
pub const LEGB_NATURAL_STEPS: &str = "legb_natural_steps";
/// The row counting inputs with a leg-B step: [`Census::legb_share`]'s denominator.
pub const LEGB_INPUTS: &str = "legb_inputs";
/// The row counting inputs with a natural policy mirror: the share's numerator.
pub const LEGB_NATURAL_INPUTS: &str = "legb_natural_inputs";
/// The row counting inputs that replayed one of the target's public seeds byte for byte.
pub const LEGB_SEED_REPLAYS: &str = "legb_seed_replays";
/// The row counting inputs that were not a public seed: mutated executions.
pub const LEGB_MUTATED: &str = "legb_mutated";
/// The share's numerator, the inputs that were a public seed (OD-L3-117 guard (c)).
pub const LEGB_NATURAL_SEED_INPUTS: &str = "legb_natural_seed_inputs";
/// The share's numerator, the mutated executions (OD-L3-117 guard (c)).
pub const LEGB_NATURAL_MUTATED_INPUTS: &str = "legb_natural_mutated_inputs";
/// The row counting leg-B inputs run under the `hold` W schedule (OD-L3-118); the rest ran
/// under `drain`.
pub const LEGB_HOLD: &str = "legb_hold";
/// The share's numerator, the inputs run under `drain` (OD-L3-119 item 3).
pub const LEGB_NATURAL_DRAIN_INPUTS: &str = "legb_natural_drain_inputs";
/// The share's numerator, the inputs run under `hold` (OD-L3-119 item 3).
pub const LEGB_NATURAL_HOLD_INPUTS: &str = "legb_natural_hold_inputs";

fn bump<K: Ord>(map: &mut BTreeMap<K, u64>, key: K) {
    *map.entry(key).or_insert(0) += 1;
}

fn add_counts<K: Ord>(into: &mut BTreeMap<K, u64>, from: BTreeMap<K, u64>) {
    for (key, n) in from {
        *into.entry(key).or_insert(0) += n;
    }
}

pub fn class_name(class: &ReplyClass) -> &'static str {
    match class {
        ReplyClass::Ok => "ok",
        ReplyClass::Duplicate => "duplicate",
        ReplyClass::Inconclusive => "inconclusive",
        ReplyClass::Reject(_) => "reject",
        ReplyClass::Down => "down",
    }
}

impl Census {
    pub fn op(&mut self, kind: &'static str, class: &ReplyClass) {
        bump(&mut self.ops, (kind, class_name(class)));
        if let ReplyClass::Reject(r) = class {
            bump(&mut self.rejects, reject_key(r));
        }
    }

    /// An op the second system did not serve (O2b-3b's Diff mode): its input ends there, out of
    /// domain, counted under `(kind, key)` ([`Unsupported::key`](super::run::Unsupported::key)).
    pub fn unsupported(&mut self, kind: &'static str, key: &'static str) {
        bump(&mut self.ops, (kind, key));
    }

    /// What the record plane of a step judged the same left out or checked apart
    /// ([`Skipped`](super::compare::Skipped)): `org=verifydb` outcome records, both sides' (D17),
    /// under the row `verifydb`, the record pairs whose unwritten keys it checked (OD-L3-74),
    /// under `unwritten checked`, and its paired `C`s, those whose evo keys it compared and Core's
    /// evo values outside their `null` era (OD-L3-77), under `paired Cs`, `evo compared` and
    /// `evo non-null`.
    pub fn skipped(&mut self, skipped: super::compare::Skipped) {
        for (row, n) in [
            ("verifydb", skipped.verifydb),
            ("unwritten checked", skipped.unwritten_checked),
            ("paired Cs", skipped.c_paired),
            ("evo compared", skipped.evo_compared),
            ("evo non-null", skipped.evo_non_null),
        ] {
            if n > 0 {
                *self.rows.entry(row).or_insert(0) += n;
            }
        }
    }

    pub fn row(&mut self, name: &'static str) {
        bump(&mut self.rows, name);
    }

    pub fn avoided(&mut self, reason: &'static str) {
        bump(&mut self.avoided, reason);
    }

    pub fn deferred_restart(&mut self) {
        self.deferred_restarts += 1;
    }

    pub fn unexpected(&mut self, what: String) {
        bump(&mut self.unexpected, what);
    }

    /// Credit a scored reply to the site its expectation named (O2b-4 §4.2): `site` is the
    /// (reason, occurrence) [`Named::site`](super::generate::Named::site) gave, and the reply
    /// must carry the reason whole (`carries`: a reject that is it or goes on past it with a
    /// character no reason continues with, or a `duplicate` answer). A reply with no named site
    /// credits nothing, whatever its text.
    pub fn credit(&mut self, site: Option<(&'static str, &'static str)>, class: &ReplyClass) {
        let Some((reason, key)) = site else {
            return;
        };
        let text = match class {
            ReplyClass::Reject(r) => r.as_str(),
            ReplyClass::Duplicate => "duplicate",
            _ => return,
        };
        if !carries(text, reason) {
            return;
        }
        bump(&mut self.inventory, key.to_owned());
        if let Some(&(_, wrapper)) = WRAPPERS.iter().find(|(callee, _)| *callee == function_of(key))
        {
            bump(&mut self.inventory, wrapper.to_owned());
        }
        if key == CIS_SCRIPT
            && let Some(error) = text.strip_prefix(CIS_REASON).and_then(|e| e.strip_suffix(')'))
            && !error.is_empty()
        {
            bump(&mut self.inventory, format!("script:{error}"));
        }
    }

    /// A leg B refused open whose cleanup differed, by the node's first unmatched label.
    pub fn legb_refused_open(&mut self, label: String) {
        bump(&mut self.legb_refused_open, label);
    }

    /// Leg B's share (O2b-4 §2.7 as OD-L3-114 amends it): the inputs with a natural policy
    /// mirror over the inputs with a leg-B step ([`Totals::input`] counts them); `None` with
    /// none.
    pub fn legb_share(&self) -> Option<f64> {
        self.ratio(LEGB_NATURAL_INPUTS, LEGB_INPUTS)
    }

    /// The per-step ratio, reported only: the leg-B steps with a natural policy mirror over all
    /// leg-B steps; `None` with none.
    pub fn legb_step_share(&self) -> Option<f64> {
        self.ratio(LEGB_NATURAL_STEPS, LEGB_STEPS)
    }

    fn ratio(&self, part: &str, all: &str) -> Option<f64> {
        let row = |r| self.rows.get(r).copied().unwrap_or(0);
        (row(all) > 0).then(|| row(part) as f64 / row(all) as f64)
    }

    /// One input's census done: its leg-B steps and natural mirrors count it once each toward
    /// the share, and a natural input counts as a seed replay or a mutated execution when the
    /// fuzz target marked which it was ([`LEGB_SEED_REPLAYS`], [`LEGB_MUTATED`]), and as a
    /// `drain` or a `hold` input by its W schedule ([`LEGB_HOLD`]).
    fn close_input(&mut self) {
        let has = |c: &Census, r| c.rows.get(r).is_some_and(|&n| n > 0);
        if has(self, LEGB_STEPS) {
            self.row(LEGB_INPUTS);
        }
        if has(self, LEGB_NATURAL_STEPS) {
            self.row(LEGB_NATURAL_INPUTS);
            if has(self, LEGB_SEED_REPLAYS) {
                self.row(LEGB_NATURAL_SEED_INPUTS);
            } else if has(self, LEGB_MUTATED) {
                self.row(LEGB_NATURAL_MUTATED_INPUTS);
            }
            self.row(if has(self, LEGB_HOLD) {
                LEGB_NATURAL_HOLD_INPUTS
            } else {
                LEGB_NATURAL_DRAIN_INPUTS
            });
        }
    }

    fn merge(&mut self, other: Census) {
        add_counts(&mut self.ops, other.ops);
        add_counts(&mut self.rejects, other.rejects);
        add_counts(&mut self.rows, other.rows);
        add_counts(&mut self.avoided, other.avoided);
        add_counts(&mut self.unexpected, other.unexpected);
        add_counts(&mut self.inventory, other.inventory);
        add_counts(&mut self.legb_refused_open, other.legb_refused_open);
        self.deferred_restarts += other.deferred_restarts;
    }
}

/// The run's totals.
#[derive(Clone, Debug, Default)]
pub struct Totals {
    pub inputs: u64,
    pub discarded: u64,
    /// Discards by the first tie site: Core's (D30; O2b-1 I37's sites 1–14), or a harness site
    /// (`h1`, OD-L3-97).
    pub discard_sites: BTreeMap<TieSite, u64>,
    /// Discards by the op kind that fired the taint.
    pub discard_ops: BTreeMap<&'static str, u64>,
    /// CL_ENFORCE ops run, and the discards site 13 caused (O2b-1 I38).
    pub cl_enforce: u64,
    pub site13: u64,
    /// Inputs replayed in the same process, and replays whose reports differed.
    pub replays: u64,
    pub replay_mismatches: u64,
    /// Scored inputs only.
    pub census: Census,
    /// The public seeds a leg-B target tells its inputs apart from; 0 for a target without leg B
    /// (a leg-B target with none refuses to start), and then no input is split into seed replays
    /// and mutated executions.
    pub legb_seeds: usize,
}

impl Totals {
    /// Fold an input in. `discard` is the op kind and the tie sites of the step that fired the
    /// taint, if it fired.
    pub fn input(&mut self, mut census: Census, discard: Option<(&'static str, &[TieSite])>) {
        self.inputs += 1;
        census.close_input();
        self.cl_enforce +=
            census.ops.iter().filter(|((k, _), _)| *k == "cl_enforce").map(|(_, n)| n).sum::<u64>();
        match discard {
            None => self.census.merge(census),
            Some((op, sites)) => {
                self.discarded += 1;
                bump(&mut self.discard_ops, op);
                if let Some(&site) = sites.first() {
                    bump(&mut self.discard_sites, site);
                }
                if op == "cl_enforce" && sites.contains(&TieSite::Core(13)) {
                    self.site13 += 1;
                }
            }
        }
    }

    /// The tie-discard rate (gated at 1%, O2b §6.5).
    pub fn discard_rate(&self) -> f64 {
        if self.inputs == 0 {
            0.0
        } else {
            self.discarded as f64 / self.inputs as f64
        }
    }

    /// The stats file's census. Every chain target writes the four `legb_*` keys: for a target
    /// without leg B, `legb_share` and `legb_step_share` are null, `legb_seeds` is 0 and
    /// `legb_refused_open` is empty, meaning "no leg B here", not a failed seed load.
    pub fn to_json(&self) -> Value {
        let c = &self.census;
        let ops: BTreeMap<String, u64> =
            c.ops.iter().map(|((k, cl), n)| (format!("{k}:{cl}"), *n)).collect();
        json!({
            "inputs": self.inputs,
            "discarded": self.discarded,
            "discard_rate": self.discard_rate(),
            "discard_sites": self.discard_sites.iter().map(|(s, n)| (s.to_string(), *n)).collect::<BTreeMap<_, _>>(),
            "discard_ops": self.discard_ops,
            "cl_enforce": self.cl_enforce,
            "site13": self.site13,
            "replays": self.replays,
            "replay_mismatches": self.replay_mismatches,
            "ops": ops,
            "rejects": c.rejects,
            "rows": c.rows,
            "avoided": c.avoided,
            "deferred_restarts": c.deferred_restarts,
            "unexpected": c.unexpected,
            "inventory": c.inventory,
            "legb_refused_open": c.legb_refused_open,
            "legb_share": c.legb_share(),
            "legb_step_share": c.legb_step_share(),
            "legb_seeds": self.legb_seeds,
        })
    }
}

/// A reject's census key: its text with every block hash in it (`Must submit previous header
/// (<hash>) first`) replaced by `<hash>`.
fn reject_key(reject: &str) -> String {
    let mut key = String::with_capacity(reject.len());
    let mut rest = reject;
    while !rest.is_empty() {
        let hex = rest.bytes().take_while(u8::is_ascii_hexdigit).count();
        if hex == 64 {
            key.push_str("<hash>");
            rest = &rest[64..];
        } else {
            let skip = hex.max(1);
            let end = rest.char_indices().nth(skip).map_or(rest.len(), |(i, _)| i);
            key.push_str(&rest[..end]);
            rest = &rest[end..];
        }
    }
    key
}

/// The replay of a certify run a census is evidence for (`NODE_L1_CHAIN_PHASE`, O2b-4 §4.6): the
/// replay before the window, which run.sh gates on, or the one after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    PreReplay,
    PostReplay,
}

impl Phase {
    pub fn name(self) -> &'static str {
        match self {
            Phase::PreReplay => "pre-replay",
            Phase::PostReplay => "post-replay",
        }
    }

    pub fn parse(name: &str) -> Option<Phase> {
        [Phase::PreReplay, Phase::PostReplay].into_iter().find(|p| p.name() == name)
    }
}

/// Which fold a census comes from: one every so many inputs (and the warm's), the process's exit,
/// or a session given up after a shim fault.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fold {
    Periodic,
    Exit,
    Replaced,
}

/// What a [`Writer`] writes and renames through: the file system, or a test's.
pub trait Sink {
    fn write(&mut self, path: &Path, text: &str) -> std::io::Result<()>;
    fn rename(&mut self, from: &Path, to: &Path) -> std::io::Result<()>;
}

/// The file system.
pub struct Fs;

impl Sink for Fs {
    fn write(&mut self, path: &Path, text: &str) -> std::io::Result<()> {
        std::fs::write(path, text)
    }

    fn rename(&mut self, from: &Path, to: &Path) -> std::io::Result<()> {
        std::fs::rename(from, to)
    }
}

/// The file a failed phase census leaves beside its destination.
pub const FAILED_MARKER: &str = "census.failed";

/// Where a process's census goes.
///
/// Without a phase it is the stats file, written in place at every fold, a failed write ignored
/// (O2b §6.5). With one it is a certify replay's evidence (O2b-4 §4.6), written only to its
/// destination: each fold writes `<dest>.tmp-<pid>` and renames it over `dest`; the census names
/// the phase and the writer's provenance (the schedule, `NODE_FUZZ_RUN`, `NODE_FUZZ_PIN`), and
/// `complete` is true only in the exit fold's ([`Writer::stamp`]). Any failure, or a session given up after a shim
/// fault, removes `dest`, leaves [`FAILED_MARKER`] beside it, and stops the writer for good: a
/// failed write never leaves an older census in place to pass for the last. (Where even the
/// removal fails, any older census left has `complete: false`, which no reader takes.)
pub struct Writer<S = Fs> {
    dest: PathBuf,
    phase: Option<Phase>,
    provenance: Map<String, Value>,
    failed: bool,
    sink: S,
}

impl<S: Sink> Writer<S> {
    pub fn new(
        dest: PathBuf,
        phase: Option<Phase>,
        provenance: Map<String, Value>,
        sink: S,
    ) -> Self {
        Writer {
            dest,
            phase,
            provenance,
            failed: false,
            sink,
        }
    }

    pub fn dest(&self) -> &Path {
        &self.dest
    }

    /// Whether a phase census failed: nothing more is written.
    pub fn failed(&self) -> bool {
        self.failed
    }

    /// The keys a fold's census gains (`executions`, and with a phase the phase, the provenance
    /// and `complete`, which only the exit fold's sets), or None when the fold writes nothing: a
    /// phase census already failed, or a session given up, which fails it.
    pub fn stamp(&mut self, fold: Fold, executions: u64) -> Option<Map<String, Value>> {
        let mut keys = Map::new();
        keys.insert("executions".into(), executions.into());
        let Some(phase) = self.phase else {
            return Some(keys);
        };
        if fold == Fold::Replaced {
            self.fail();
        }
        if self.failed {
            return None;
        }
        keys.insert("phase".into(), phase.name().into());
        keys.extend(self.provenance.clone());
        keys.insert("complete".into(), (fold == Fold::Exit).into());
        Some(keys)
    }

    /// A census `stamp` allowed: in place without a phase (a failed write ignored), else by
    /// rename, failing the census if either step fails.
    pub fn write(&mut self, text: &str) {
        if self.phase.is_none() {
            let _ = self.sink.write(&self.dest, text);
            return;
        }
        if self.failed {
            return;
        }
        let mut tmp = self.dest.clone().into_os_string();
        tmp.push(format!(".tmp-{}", std::process::id()));
        let tmp = PathBuf::from(tmp);
        if self.sink.write(&tmp, text).and_then(|()| self.sink.rename(&tmp, &self.dest)).is_err() {
            let _ = std::fs::remove_file(&tmp);
            self.fail();
        }
    }

    /// A phase census that can no longer be trusted: `dest` removed, the marker left beside it,
    /// and no later write. Nothing without a phase.
    pub fn fail(&mut self) {
        if self.phase.is_none() || self.failed {
            return;
        }
        self.failed = true;
        let _ = std::fs::remove_file(&self.dest);
        let _ = std::fs::write(self.dest.with_file_name(FAILED_MARKER), "");
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::path::Path;
    use std::rc::Rc;

    use serde_json::Map;

    use super::{
        Census, FAILED_MARKER, Fold, Fs, LEGB_HOLD, LEGB_INPUTS, LEGB_MUTATED,
        LEGB_NATURAL_DRAIN_INPUTS, LEGB_NATURAL_HOLD_INPUTS, LEGB_NATURAL_INPUTS,
        LEGB_NATURAL_MUTATED_INPUTS, LEGB_NATURAL_SEED_INPUTS, LEGB_NATURAL_STEPS,
        LEGB_SEED_REPLAYS, LEGB_STEPS, Phase, Sink, Totals, Writer,
    };

    /// The file system, failing writes or renames while the switches say so, and logging each
    /// call.
    #[derive(Default)]
    struct Switches {
        fail_write: Cell<bool>,
        fail_rename: Cell<bool>,
        calls: RefCell<Vec<String>>,
    }

    struct Flaky(Rc<Switches>);

    impl Sink for Flaky {
        fn write(&mut self, path: &Path, text: &str) -> std::io::Result<()> {
            self.0.calls.borrow_mut().push(format!("write {}", path.display()));
            if self.0.fail_write.get() {
                return Err(std::io::Error::other("injected"));
            }
            std::fs::write(path, text)
        }

        fn rename(&mut self, from: &Path, to: &Path) -> std::io::Result<()> {
            self.0.calls.borrow_mut().push(format!("rename {} {}", from.display(), to.display()));
            if self.0.fail_rename.get() {
                return Err(std::io::Error::other("injected"));
            }
            std::fs::rename(from, to)
        }
    }

    fn flaky() -> (Flaky, Rc<Switches>) {
        let switches = Rc::new(Switches::default());
        (Flaky(Rc::clone(&switches)), switches)
    }

    fn written<S: Sink>(writer: &mut Writer<S>, fold: Fold) -> bool {
        let Some(stamp) = writer.stamp(fold, 1) else {
            return false;
        };
        writer.write(&serde_json::Value::Object(stamp).to_string());
        true
    }

    /// O2b-4 §4.6 (4e2 test 1, the writer's half): a phase census is written beside its
    /// destination, as `<dest>.tmp-<pid>`, and renamed over it; `complete` only from the exit
    /// fold.
    #[test]
    fn a_phase_census_is_written_beside_its_destination_and_renamed() {
        let dir = tempfile::tempdir().expect("a directory");
        let dest = dir.path().join("census.json");
        let (sink, switches) = flaky();
        let mut writer = Writer::new(dest.clone(), Some(Phase::PreReplay), Map::new(), sink);
        assert!(written(&mut writer, Fold::Periodic));
        let tmp = format!("{}.tmp-{}", dest.display(), std::process::id());
        assert_eq!(
            *switches.calls.borrow(),
            [format!("write {tmp}"), format!("rename {tmp} {}", dest.display())]
        );
        let read = || -> serde_json::Value {
            serde_json::from_str(&std::fs::read_to_string(&dest).expect("written")).expect("JSON")
        };
        assert_eq!((&read()["complete"], &read()["phase"]), (&false.into(), &"pre-replay".into()));
        assert!(written(&mut writer, Fold::Exit));
        assert_eq!(read()["complete"], true);
    }

    /// 4e2 test 3: a failed write, or a failed rename, removes the destination and leaves the
    /// marker beside it, and the writer writes nothing more, whatever its sink then does.
    #[test]
    fn a_failed_write_removes_the_destination_and_leaves_the_marker() {
        for fail_rename in [false, true] {
            let dir = tempfile::tempdir().expect("a directory");
            let dest = dir.path().join("census.json");
            let marker = dir.path().join(FAILED_MARKER);
            let (sink, switches) = flaky();
            let mut writer = Writer::new(dest.clone(), Some(Phase::PreReplay), Map::new(), sink);
            assert!(written(&mut writer, Fold::Periodic));
            assert!(dest.exists() && !marker.exists());
            if fail_rename {
                switches.fail_rename.set(true)
            } else {
                switches.fail_write.set(true)
            }
            assert!(written(&mut writer, Fold::Periodic), "stamped, then the write failed");
            assert!(writer.failed() && !dest.exists() && marker.exists(), "{fail_rename}");
            let names: Vec<_> = std::fs::read_dir(dir.path())
                .expect("the directory")
                .map(|e| e.expect("an entry").file_name())
                .collect();
            assert_eq!(names, [FAILED_MARKER], "no temporary file left");
            switches.fail_write.set(false);
            switches.fail_rename.set(false);
            assert!(!written(&mut writer, Fold::Exit), "sticky");
            writer.write("{}");
            assert!(!dest.exists());
        }
    }

    /// Review r1 m2: on the file system, a phase census whose directory cannot be written fails
    /// and leaves nothing there, census or marker; one whose directory turns read-only after a
    /// write fails too, and the census it cannot remove has `complete: false`.
    #[test]
    fn a_read_only_directory_fails_the_phase_census() {
        use std::os::unix::fs::PermissionsExt;
        let chmod = |dir: &Path, mode| {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode)).expect("chmod");
        };
        for late in [false, true] {
            let dir = tempfile::tempdir().expect("a directory");
            let dest = dir.path().join("census.json");
            let mut writer = Writer::new(dest.clone(), Some(Phase::PreReplay), Map::new(), Fs);
            if late {
                assert!(written(&mut writer, Fold::Periodic));
                assert!(!writer.failed() && dest.exists());
            }
            chmod(dir.path(), 0o500);
            if std::fs::write(dir.path().join("probe"), "").is_ok() {
                chmod(dir.path(), 0o700);
                eprintln!(
                    "skipped: a_read_only_directory_fails_the_phase_census (euid 0: a 0500 \
                     directory is writable here)"
                );
                return;
            }
            let fold = if late {
                Fold::Exit
            } else {
                Fold::Periodic
            };
            assert!(written(&mut writer, fold), "stamped, then the write failed");
            assert!(writer.failed(), "late: {late}");
            assert!(!written(&mut writer, Fold::Exit), "sticky");
            let names: Vec<_> = std::fs::read_dir(dir.path())
                .expect("the directory")
                .map(|e| e.expect("an entry").file_name())
                .collect();
            if late {
                assert_eq!(names, ["census.json"]);
                let left: serde_json::Value =
                    serde_json::from_str(&std::fs::read_to_string(&dest).expect("left"))
                        .expect("JSON");
                assert_eq!(left["complete"], false);
            } else {
                assert!(names.is_empty(), "{names:?}");
            }
            chmod(dir.path(), 0o700);
        }
    }

    /// Without a phase a failed write is ignored and the next one still lands, in place (O2b
    /// §6.5); a replaced session's fold writes as any other.
    #[test]
    fn without_a_phase_a_failed_write_is_ignored() {
        let dir = tempfile::tempdir().expect("a directory");
        let dest = dir.path().join("x.stats.json");
        let (sink, switches) = flaky();
        let mut writer = Writer::new(dest.clone(), None, Map::new(), sink);
        switches.fail_write.set(true);
        assert!(written(&mut writer, Fold::Periodic));
        switches.fail_write.set(false);
        assert!(written(&mut writer, Fold::Replaced));
        assert!(dest.exists() && !writer.failed());
        assert!(!dir.path().join(FAILED_MARKER).exists());
        assert!(switches.calls.borrow().iter().all(|c| *c == format!("write {}", dest.display())));
    }

    mod inventory {
        use crate::chain::census::{Census, Totals};
        use crate::chain::generate::Named;
        use crate::chain::mutate::{BANK, Mode, Par};
        use crate::chain::step::ReplyClass;

        fn bank(name: &str) -> &'static crate::chain::mutate::Operator {
            BANK.iter().find(|o| o.name == name).expect("a bank operator")
        }

        fn reject(text: &str) -> ReplyClass {
            ReplyClass::Reject(text.into())
        }

        fn keys(c: &Census) -> Vec<&str> {
            c.inventory.keys().map(String::as_str).collect()
        }

        const VIN_EMPTY: &str = "tx_check.cpp:CheckTransaction#1:bad-txns-vin-empty";
        const CT_WRAPPER: &str = "validation.cpp:CheckBlock@W:CheckTransaction";

        /// OD-L3-155: a reason is carried whole. #4's `bad-prevblk` names a prefix of #5's
        /// `bad-prevblk-chainlock`: with #4 named that reply credits nothing, with #5 named #5.
        #[test]
        fn a_longer_reason_does_not_credit_the_prefix_s_site() {
            let reply = reject("bad-prevblk-chainlock");
            let four = Some(("bad-prevblk", "validation.cpp:AcceptBlockHeader#4:bad-prevblk"));
            let five = Some((
                "bad-prevblk-chainlock",
                "validation.cpp:AcceptBlockHeader#5:bad-prevblk-chainlock",
            ));
            let mut c = Census::default();
            c.credit(four, &reply);
            assert!(c.inventory.is_empty(), "#4 credited by #5's reason: {:?}", c.inventory);
            c.credit(five, &reply);
            assert_eq!(keys(&c), ["validation.cpp:AcceptBlockHeader#5:bad-prevblk-chainlock"]);
            let mut d = Census::default();
            d.credit(four, &reject("bad-prevblk"));
            d.credit(four, &reject("bad-prevblk, extra"));
            assert_eq!(d.inventory["validation.cpp:AcceptBlockHeader#4:bad-prevblk"], 2);
        }

        /// OD-L3-155's inventory pass: under the whole-reason match no site's reply carries
        /// another site's different reason (under the prefix match #5's `bad-prevblk-chainlock`
        /// carried #4's and #6's `bad-prevblk`). Sites sharing one reason (#1/#2 `duplicate`,
        /// #4/#6 `bad-prevblk`, ...) are told apart only by where the model names them.
        #[test]
        fn no_two_inventory_reasons_collide_under_the_whole_reason_match() {
            let inventory: serde_json::Value = serde_json::from_str(include_str!(
                "../../../contrib/node/l3-census/inventory-v24.0.0.json"
            ))
            .expect("the inventory");
            // A template's literal head: the reason the site's reply begins with, up to its
            // first format directive.
            let reasons: Vec<(&str, &str)> = inventory["rows"]
                .as_array()
                .expect("its rows")
                .iter()
                .filter_map(|r| {
                    let t = r["template"].as_str()?;
                    Some((r["key"].as_str().expect("a key"), t.split('%').next().unwrap_or(t)))
                })
                .filter(|(_, t)| !t.is_empty())
                .collect();
            let mut collide = Vec::new();
            for &(ka, a) in &reasons {
                for &(kb, b) in &reasons {
                    if ka != kb && a != b && super::super::carries(b, a) {
                        collide.push(format!("{kb} ({b}) carries {ka} ({a})"));
                    }
                }
            }
            assert!(collide.is_empty(), "{collide:#?}");
        }

        /// T1: an expectation naming its site, and a reply carrying the site's reason, credit the
        /// site's inventory key once per reply.
        #[test]
        fn a_site_named_reject_credits_its_inventory_row() {
            let mut c = Census::default();
            let site = Named::Op(bank("x_bip30"), Mode::Submit).site(Par::One);
            assert_eq!(
                site,
                Some(("bad-txns-BIP30", "validation.cpp:ConnectBlock#2:bad-txns-BIP30"))
            );
            c.credit(site, &reject("bad-txns-BIP30"));
            assert_eq!(keys(&c), ["validation.cpp:ConnectBlock#2:bad-txns-BIP30"]);
            c.credit(site, &reject("bad-txns-BIP30"));
            assert_eq!(c.inventory["validation.cpp:ConnectBlock#2:bad-txns-BIP30"], 2);
            let mut d = Census::default();
            let dup = Some(("duplicate", "validation.cpp:AcceptBlockHeader#1:duplicate"));
            d.credit(dup, &ReplyClass::Duplicate);
            assert_eq!(keys(&d), ["validation.cpp:AcceptBlockHeader#1:duplicate"]);
        }

        /// T2: no named site, a reply without the site's reason, a non-reject reply, an
        /// unexpected reply and a discarded input credit nothing, and crediting never touches
        /// `rejects`.
        #[test]
        fn a_reply_without_a_named_site_credits_nothing() {
            let site = Named::Op(bank("x_bip30"), Mode::Submit).site(Par::One);
            let mut c = Census::default();
            c.credit(Named::None.site(Par::One), &reject("bad-txns-BIP30"));
            c.credit(None, &reject("bad-txns-BIP30"));
            c.credit(site, &reject("bad-txns-inputs-missingorspent"));
            c.credit(site, &reject("bad-txns-BIP3"));
            for class in [ReplyClass::Ok, ReplyClass::Inconclusive, ReplyClass::Down] {
                c.credit(site, &class);
            }
            c.credit(site, &ReplyClass::Duplicate);
            c.unexpected("block: bad-txns-BIP30".into());
            assert!(c.inventory.is_empty(), "{:?}", c.inventory);
            assert!(c.rejects.is_empty(), "crediting counts no reject");

            let mut credited = Census::default();
            credited.credit(site, &reject("bad-txns-BIP30"));
            let mut totals = Totals::default();
            totals.input(credited, Some(("block", &[])));
            assert!(totals.census.inventory.is_empty(), "a discarded input credits nothing");
        }

        /// T3: a script error inline (`-par=1`) credits CheckInputScripts#2 and its
        /// `script:<error>` key; queued (`-par=4`) it credits ConnectBlock#7 and no script key.
        #[test]
        fn a_script_error_credits_cis2_and_its_script_key_on_par1_only() {
            let op = bank("se_verify");
            let text = "mandatory-script-verify-flag-failed (Script failed an OP_VERIFY operation)";
            let mut one = Census::default();
            one.credit(Named::Op(op, Mode::Submit).site(Par::One), &reject(text));
            assert_eq!(
                keys(&one),
                [
                    "script:Script failed an OP_VERIFY operation",
                    "validation.cpp:CheckInputScripts#2:mandatory-script-verify-flag-failed (%s)",
                    "validation.cpp:ConnectBlock@W:CheckInputScripts",
                ]
            );
            let mut four = Census::default();
            four.credit(
                Named::Op(op, Mode::Submit).site(Par::Four),
                &reject("block-validation-failed"),
            );
            assert_eq!(keys(&four), ["validation.cpp:ConnectBlock#7:block-validation-failed"]);
            four.credit(Named::Op(op, Mode::Submit).site(Par::Four), &reject(text));
            assert_eq!(four.inventory.len(), 1, "the queue's verdict names no script error");
        }

        /// T4: a callee's credit also credits the wrapper row that propagates it.
        #[test]
        fn a_wrapper_row_is_credited_from_its_callees_reply() {
            let mut c = Census::default();
            let site = Named::Op(bank("x_vin_empty"), Mode::Submit).site(Par::One);
            assert_eq!(site, Some(("bad-txns-vin-empty", VIN_EMPTY)));
            c.credit(site, &reject("bad-txns-vin-empty"));
            assert_eq!(keys(&c), [VIN_EMPTY, CT_WRAPPER]);
            let mut header = Census::default();
            header.credit(
                Some(("bad-prevblk", "validation.cpp:AcceptBlockHeader#4:bad-prevblk")),
                &reject("bad-prevblk"),
            );
            assert_eq!(keys(&header), ["validation.cpp:AcceptBlockHeader#4:bad-prevblk"]);
        }

        /// T5: the stats JSON gains only `inventory`, a sorted map, over integration's keys.
        #[test]
        fn the_stats_json_only_gains_inventory() {
            let mut totals = Totals::default();
            let mut c = Census::default();
            c.credit(Some(("bad-txns-vin-empty", VIN_EMPTY)), &reject("bad-txns-vin-empty"));
            totals.input(c, None);
            let json = totals.to_json();
            let mut keys: Vec<&str> =
                json.as_object().expect("an object").keys().map(String::as_str).collect();
            keys.sort_unstable();
            let mut want = vec![
                "inputs",
                "discarded",
                "discard_rate",
                "discard_sites",
                "discard_ops",
                "cl_enforce",
                "site13",
                "replays",
                "replay_mismatches",
                "ops",
                "rejects",
                "rows",
                "avoided",
                "deferred_restarts",
                "unexpected",
                "legb_refused_open",
                "legb_share",
                "legb_step_share",
                "legb_seeds",
                "inventory",
            ];
            want.sort_unstable();
            assert_eq!(keys, want);
            let inventory: Vec<&str> =
                json["inventory"].as_object().expect("a map").keys().map(String::as_str).collect();
            assert_eq!(inventory, [VIN_EMPTY, CT_WRAPPER]);
            assert_eq!(json["inventory"][VIN_EMPTY], 1);
        }
    }

    /// The share counts inputs, each once however many natural mirrors it had, over the inputs
    /// with a leg-B step; the step ratio counts steps; an input with no leg-B step is in neither.
    #[test]
    fn the_legb_share_counts_inputs_and_the_step_share_steps() {
        let input = |steps: usize, natural: usize| {
            let mut c = Census::default();
            (0..steps).for_each(|_| c.row(LEGB_STEPS));
            (0..natural).for_each(|_| c.row(LEGB_NATURAL_STEPS));
            c
        };
        let mut totals = Totals::default();
        assert_eq!(totals.census.legb_share(), None);
        totals.input(input(10, 3), None);
        totals.input(input(10, 0), None);
        totals.input(input(0, 0), None);
        totals.input(input(20, 1), None);
        assert_eq!(totals.census.legb_share(), Some(2.0 / 3.0));
        assert_eq!(totals.census.legb_step_share(), Some(4.0 / 40.0));
        assert_eq!(totals.census.rows[LEGB_INPUTS], 3);
        assert_eq!(totals.census.rows[LEGB_NATURAL_INPUTS], 2);
        assert!(!totals.census.rows.contains_key(LEGB_NATURAL_SEED_INPUTS));
        assert!(!totals.census.rows.contains_key(LEGB_NATURAL_MUTATED_INPUTS));
    }

    /// A natural input counts once as a seed replay or a mutated execution, by the target's
    /// mark; one with no natural mirror counts as neither.
    #[test]
    fn natural_inputs_split_into_seed_replays_and_mutated_executions() {
        let input = |natural: bool, mark: &'static str| {
            let mut c = Census::default();
            c.row(LEGB_STEPS);
            if natural {
                (0..2).for_each(|_| c.row(LEGB_NATURAL_STEPS));
            }
            c.row(mark);
            c
        };
        let mut totals = Totals::default();
        totals.input(input(true, LEGB_SEED_REPLAYS), None);
        totals.input(input(true, LEGB_MUTATED), None);
        totals.input(input(true, LEGB_MUTATED), None);
        totals.input(input(false, LEGB_SEED_REPLAYS), None);
        totals.input(input(false, LEGB_MUTATED), None);
        let rows = &totals.census.rows;
        assert_eq!(rows[LEGB_NATURAL_INPUTS], 3);
        assert_eq!(rows[LEGB_NATURAL_SEED_INPUTS], 1);
        assert_eq!(rows[LEGB_NATURAL_MUTATED_INPUTS], 2);
        assert_eq!(rows[LEGB_SEED_REPLAYS], 2);
        assert_eq!(rows[LEGB_MUTATED], 3);
        assert_eq!(totals.census.legb_share(), Some(3.0 / 5.0));
    }

    /// A natural input counts once under its W schedule: `hold` if the runner marked it, else
    /// `drain`; one with no natural mirror counts under neither.
    #[test]
    fn natural_inputs_split_by_w_schedule() {
        let input = |natural: bool, hold: bool| {
            let mut c = Census::default();
            c.row(LEGB_STEPS);
            if natural {
                c.row(LEGB_NATURAL_STEPS);
            }
            if hold {
                c.row(LEGB_HOLD);
            }
            c
        };
        let mut totals = Totals::default();
        totals.input(input(true, false), None);
        totals.input(input(true, true), None);
        totals.input(input(true, true), None);
        totals.input(input(false, true), None);
        totals.input(input(false, false), None);
        let rows = &totals.census.rows;
        assert_eq!(rows[LEGB_NATURAL_INPUTS], 3);
        assert_eq!(rows[LEGB_NATURAL_DRAIN_INPUTS], 1);
        assert_eq!(rows[LEGB_NATURAL_HOLD_INPUTS], 2);
        assert_eq!(rows[LEGB_HOLD], 3);
    }
}
