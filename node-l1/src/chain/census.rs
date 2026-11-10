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
}

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

    fn merge(&mut self, other: Census) {
        add_counts(&mut self.ops, other.ops);
        add_counts(&mut self.rejects, other.rejects);
        add_counts(&mut self.rows, other.rows);
        add_counts(&mut self.avoided, other.avoided);
        add_counts(&mut self.unexpected, other.unexpected);
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
}

impl Totals {
    /// Fold an input in. `discard` is the op kind and the tie sites of the step that fired the
    /// taint, if it fired.
    pub fn input(&mut self, census: Census, discard: Option<(&'static str, &[TieSite])>) {
        self.inputs += 1;
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

    use super::{FAILED_MARKER, Fold, Fs, Phase, Sink, Writer};

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
}
