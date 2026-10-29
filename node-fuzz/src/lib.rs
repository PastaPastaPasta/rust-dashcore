// EMBARGO-1
//! The harness every fuzz target uses (CS0 §9.3): the start-up check, then each input through the
//! target's check on the runner's pin. A divergence goes to the private sink and the run
//! continues; nothing about an input is printed.
//!
//! Two kinds of target (O3; `targets.toml` lists every one):
//! - **L1** (`l1_target!`): a `node-l1` comparison against Dash Core's oracle on the pin.
//! - **Model** (`model_target!`, DEC-59 Q1): a check of node code against a reference model,
//!   registered in [`models::MODELS`]. The pin names the Core release the target runs against.

#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::use_debug,
    clippy::dbg_macro,
    clippy::print_stdout
)]

pub mod models;

use std::sync::atomic::{AtomicU64, Ordering};

use dash_oracle_sys::Pin;
use node_l1::{Compared, Divergence, Outcome};
use node_sink::report;
use node_sink::sink::{Sink, fuzz_init};

#[doc(hidden)]
pub use libfuzzer_sys;
use libfuzzer_sys::Corpus;

/// Divergences recorded before a run stops: a systematic divergence is one finding, and the run
/// fails either way.
const MAX_DIVERGENCES: u64 = 100;

/// A file in the run directory that the runner's minimisations write: a divergence in the field
/// it names (its first line), and on the command it names (an optional second line, `cmd <name>`:
/// a model target's divergence carries one), aborts, so libFuzzer's crash minimisation keeps the
/// divergence class (a `minimize-divergence` job), and every other divergence is ignored (a
/// `minimize` job writes `-`, which names no field, so a mutation that diverges does not fail a
/// crash minimisation). A file, not the environment: a divergence's field must not show in /proc
/// (PLAN §7.5).
pub const ABORT_FIELD_FILE: &str = "abort-field";

/// What a minimisation aborts on (the contents of [`ABORT_FIELD_FILE`]).
#[derive(Debug, PartialEq, Eq)]
struct AbortRule {
    field: String,
    cmd: Option<String>,
}

impl AbortRule {
    fn parse(text: &str) -> AbortRule {
        let mut lines = text.lines();
        AbortRule {
            field: lines.next().unwrap_or_default().trim_end().to_owned(),
            cmd: lines.find_map(|l| l.strip_prefix("cmd ")).map(|c| c.trim_end().to_owned()),
        }
    }

    /// Whether a divergence in `field` on `cmd` is the class being minimised: the same field and,
    /// when the rule names a command, the same command (a shrink onto another command is another
    /// cluster's finding).
    fn aborts(&self, field: &str, cmd: Option<&str>) -> bool {
        self.field == field && self.cmd.as_deref().is_none_or(|want| cmd == Some(want))
    }
}

/// Defines an L1 fuzz target: `l1_target!("name")` for a registered `node-l1` target, or
/// `l1_target!("name", compare)` with an explicit comparison (the runner's self-test).
#[macro_export]
macro_rules! l1_target {
    ($name:literal $(, $compare:expr)?) => {
        static HARNESS: ::std::sync::OnceLock<$crate::Harness> = ::std::sync::OnceLock::new();

        $crate::libfuzzer_sys::fuzz_target!(
            init: {
                let _ = HARNESS.set($crate::l1_target!(@init $name $(, $compare)?));
            },
            |data: &[u8]| -> $crate::libfuzzer_sys::Corpus {
                HARNESS
                    .get()
                    .map_or($crate::libfuzzer_sys::Corpus::Keep, |harness| harness.run(data))
            }
        );
    };
    (@init $name:literal) => { $crate::Harness::init($name) };
    (@init $name:literal, $compare:expr) => { $crate::Harness::init_with($name, $compare) };
}

/// Defines a model fuzz target: `model_target!("name")` for a target registered in
/// [`models::MODELS`].
#[macro_export]
macro_rules! model_target {
    ($name:literal) => {
        static HARNESS: ::std::sync::OnceLock<$crate::Harness> = ::std::sync::OnceLock::new();

        $crate::libfuzzer_sys::fuzz_target!(
            init: {
                let _ = HARNESS.set($crate::Harness::init_model($name));
            },
            |data: &[u8]| -> $crate::libfuzzer_sys::Corpus {
                HARNESS
                    .get()
                    .map_or($crate::libfuzzer_sys::Corpus::Keep, |harness| harness.run(data))
            }
        );
    };
}

/// What a target checks each input with.
enum Check {
    L1(Pin, fn(&[u8], Pin) -> Compared),
    Model(fn(&[u8]) -> Result<(), Divergence>),
}

impl Check {
    /// The input's outcome (a model check that passes is an agreement).
    fn run(&self, data: &[u8]) -> Compared {
        match self {
            Check::L1(pin, compare) => compare(data, *pin),
            Check::Model(check) => check(data).map(|()| Outcome::BothAccept),
        }
    }
}

/// Whether libFuzzer's arguments make this process one that only starts others and runs no
/// input: `-jobs` or `-fork` above 0, `-minimize_crash=1` (its children run the input), or a
/// `-merge=1` or `-set_cover_merge=1` without the child's control file. Its initialisation hook
/// runs all the same.
fn dispatches_only(args: impl Iterator<Item = String>) -> bool {
    let args: Vec<String> = args.collect();
    let above_zero = |flag: &str| {
        args.iter()
            .filter_map(|a| a.strip_prefix(flag))
            .any(|v| v.parse::<u64>().is_ok_and(|n| n > 0))
    };
    let has = |prefix: &str| args.iter().any(|a| a.starts_with(prefix));
    above_zero("-jobs=")
        || above_zero("-fork=")
        || has("-minimize_crash=1")
        || ((has("-merge=1") || has("-set_cover_merge=1")) && !has("-merge_control_file="))
}

/// One running target.
pub struct Harness {
    target: &'static str,
    sink: Sink,
    pin: &'static str,
    check: Check,
    abort: Option<AbortRule>,
    divergences: AtomicU64,
    strict_flags: AtomicU64,
}

impl Harness {
    /// For libFuzzer's initialisation hook: exits (before any input runs) unless the runner
    /// started this target, or if `target` is not registered in `node_l1::TARGETS`, or the
    /// runner's pin is not one the target compares on; then runs the target's start, unless
    /// this process only dispatches others.
    pub fn init(target: &'static str) -> Harness {
        let Some(registered) = node_l1::target(target) else {
            report(&format!("{target}: refusing to run: not a registered L1 target"));
            std::process::exit(2);
        };
        let harness = Harness::init_with(target, registered.compare);
        let Some(pin) = registered.pins.iter().find(|pin| pin.name() == harness.pin) else {
            report(&format!("{target}: refusing to run: the target does not compare on the pin"));
            std::process::exit(2);
        };
        if let Some(start) = registered.start.filter(|_| !dispatches_only(std::env::args())) {
            start(*pin);
        }
        harness
    }

    /// As [`Harness::init`], with an explicit comparison (the runner's self-test).
    pub fn init_with(target: &'static str, compare: fn(&[u8], Pin) -> Compared) -> Harness {
        let (sink, pin) = fuzz_init(target);
        let Some(pin) = node_l1::pin_named(&pin) else {
            report(&format!("{target}: refusing to run: the pin is not linked"));
            std::process::exit(2);
        };
        Harness::new(target, sink, pin.name(), Check::L1(pin, compare))
    }

    /// For a model target's initialisation hook: as [`Harness::init`], for a target registered
    /// in [`models::MODELS`]. The runner's pin must be a linked pin.
    pub fn init_model(target: &'static str) -> Harness {
        let Some(registered) = models::model(target) else {
            report(&format!("{target}: refusing to run: not a registered model target"));
            std::process::exit(2);
        };
        let (sink, pin) = fuzz_init(target);
        let Some(pin) = node_l1::pin_named(&pin) else {
            report(&format!("{target}: refusing to run: the pin is not linked"));
            std::process::exit(2);
        };
        Harness::new(target, sink, pin.name(), Check::Model(registered.check))
    }

    fn new(target: &'static str, sink: Sink, pin: &'static str, check: Check) -> Harness {
        let abort = std::fs::read_to_string(sink.run_dir().join(ABORT_FIELD_FILE))
            .ok()
            .map(|text| AbortRule::parse(&text));
        Harness {
            target,
            sink,
            pin,
            check,
            abort,
            divergences: AtomicU64::new(0),
            strict_flags: AtomicU64::new(0),
        }
    }

    /// Checks one input. A divergence is recorded and the run continues, up to
    /// [`MAX_DIVERGENCES`]; a sink that cannot be written stops the run. Under the runner's
    /// minimisations ([`ABORT_FIELD_FILE`]) a divergence in the named field aborts instead and
    /// other divergences are ignored. Inputs of the declared strictness ([`Outcome::StrictFlag`])
    /// are counted apart: the run's log gets the count at each power of two. A discarded input
    /// ([`Outcome::Discarded`]) is kept out of the corpus.
    pub fn run(&self, data: &[u8]) -> Corpus {
        models::clear_command();
        let divergence = match self.check.run(data) {
            Ok(Outcome::StrictFlag) => {
                let n = self.strict_flags.fetch_add(1, Ordering::Relaxed) + 1;
                if n.is_power_of_two() {
                    report(&format!(
                        "{}: {n} inputs Core reads with a flag byte the node refuses (A-12, DEC-98)",
                        self.target
                    ));
                }
                return Corpus::Keep;
            }
            Ok(Outcome::Discarded) => return Corpus::Reject,
            Ok(_) => return Corpus::Keep,
            Err(divergence) => divergence,
        };
        let cmd = models::noted_command();
        if let Some(rule) = &self.abort {
            if rule.aborts(divergence.field, cmd.as_deref()) {
                std::process::abort();
            }
            return Corpus::Keep;
        }
        if self.sink.record_with_cmd(data, self.pin, divergence.field, cmd.as_deref()).is_err() {
            report(&format!("{}: sink write failed", self.target));
            std::process::abort();
        }
        if self.divergences.fetch_add(1, Ordering::Relaxed) + 1 >= MAX_DIVERGENCES {
            report(&format!("{}: divergence limit reached", self.target));
            std::process::exit(3);
        }
        Corpus::Keep
    }
}

#[cfg(test)]
mod tests {
    use super::{ABORT_FIELD_FILE, AbortRule, Check, Divergence, Harness, dispatches_only, models};
    use node_sink::sink::Sink;

    /// A model that diverges in `log line` on the command its input's first byte picks.
    fn model_on_first_byte(data: &[u8]) -> Result<(), Divergence> {
        let cmd = match data.first() {
            Some(0) => dash_p2p::CommandString::from_static("ping"),
            Some(1) => dash_p2p::CommandString::from_static("pong"),
            _ => dash_p2p::CommandString::from_bytes([0xaa; 12]),
        };
        models::note_command(&cmd);
        Err(Divergence {
            field: "log line",
        })
    }

    #[test]
    #[allow(clippy::expect_used, clippy::unwrap_used)]
    fn a_model_divergence_is_recorded_with_its_command_and_a_minimisation_keeps_it() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("node-fuzz-cmd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("fuzz")).unwrap();
        std::fs::set_permissions(root.join("fuzz"), std::fs::Permissions::from_mode(0o700))
            .unwrap();
        // SAFETY: no other test of this crate reads or sets `DASH_EMBARGO_DIR`; the only other
        // environment reads are `temp_dir` lookups, which never see this variable.
        unsafe { std::env::set_var(node_sink::DIR_VAR, &root) };
        let sidecar = |h: &Harness, data: &[u8]| {
            let name = format!("{}.txt", node_sink::sha256_hex(data));
            std::fs::read_to_string(h.sink.run_dir().join("divergences").join(name))
                .unwrap_or_default()
        };

        let sink = Sink::open("p2p_decode_model", "recording").unwrap();
        let h = Harness::new("p2p_decode_model", sink, "v24.0", Check::Model(model_on_first_byte));
        for input in [&[0u8][..], &[1], &[9]] {
            h.run(input);
        }
        assert!(sidecar(&h, &[0]).ends_with("field log line\ncmd ping\n"));
        assert!(sidecar(&h, &[1]).ends_with("field log line\ncmd pong\n"));
        assert!(sidecar(&h, &[9]).ends_with("field log line\ncmd unknown\n"));

        // A minimisation of `log line` on `ping`: its rule file keeps the divergences of other
        // commands from aborting (they are ignored, nothing is recorded).
        let sink = Sink::open("p2p_decode_model", "minimising").unwrap();
        std::fs::write(sink.run_dir().join(ABORT_FIELD_FILE), "log line\ncmd ping\n").unwrap();
        let h = Harness::new("p2p_decode_model", sink, "v24.0", Check::Model(model_on_first_byte));
        h.run(&[1]);
        h.run(&[9]);
        assert_eq!(std::fs::read_dir(h.sink.run_dir().join("divergences")).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_abort_rule_with_a_command_aborts_only_on_that_command() {
        let field_only = AbortRule::parse("log line\n");
        assert!(field_only.aborts("log line", None));
        assert!(field_only.aborts("log line", Some("ping")));
        assert!(!field_only.aborts("verdict", Some("ping")));
        let on_ping = AbortRule::parse("log line\ncmd ping\n");
        assert_eq!(on_ping.cmd.as_deref(), Some("ping"));
        assert!(on_ping.aborts("log line", Some("ping")));
        assert!(!on_ping.aborts("log line", Some("pong")));
        assert!(!on_ping.aborts("log line", None));
        assert!(!on_ping.aborts("verdict", Some("ping")));
        // A crash minimisation's `-` names no field.
        assert!(!AbortRule::parse("-\n").aborts("log line", Some("ping")));
    }

    fn args(list: &[&str]) -> impl Iterator<Item = String> {
        list.iter().map(|a| a.to_string()).collect::<Vec<_>>().into_iter()
    }

    #[test]
    fn only_a_dispatching_process_skips_the_start() {
        assert!(!dispatches_only(args(&["fuzz", "-runs=0", "corpus"])));
        assert!(dispatches_only(args(&["fuzz", "-jobs=3", "-workers=3", "corpus"])));
        assert!(!dispatches_only(args(&["fuzz", "-jobs=0"])));
        assert!(dispatches_only(args(&["fuzz", "-fork=2"])));
        assert!(dispatches_only(args(&["fuzz", "-merge=1", "out", "in"])));
        assert!(!dispatches_only(args(&["fuzz", "-merge=1", "-merge_control_file=/x", "out"])));
        assert!(dispatches_only(args(&["fuzz", "-set_cover_merge=1", "out", "in"])));
        assert!(!dispatches_only(args(&["fuzz", "-set_cover_merge=1", "-merge_control_file=/x"])));
        assert!(dispatches_only(args(&["fuzz", "-minimize_crash=1", "-runs=10000", "crash"])));
        assert!(!dispatches_only(args(&["fuzz", "-minimize_crash_internal_step=1", "crash"])));
    }
}
