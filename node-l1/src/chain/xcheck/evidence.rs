//! The X-check's evidence (O2b-4 PLAN §5.5): `O2b-xcheck-M3.json` and `.md`. Provenance, counts
//! and plane dispositions only: program labels, never input bytes.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use serde_json::{Value, json};

use super::sample::{CONTEXTS, Category, Context, END_KINDS, Minimums, Tally};
use super::spork::{self, Row};
use super::{Agreement, Binary, Disagreement, private_dir};

pub const NAME: &str = "O2b-xcheck-M3";

/// The schedules an M3 run covers, all of them: a narrower run is not the M3 gate.
pub const M3_SCHEDULES: [&str; 3] = ["S0", "S1", "S2"];

/// The `(pair, plane)` rows each schedule must have compared equal at least once: a row that
/// never ran is missing coverage, not agreement.
pub const EXPECTED_ROWS: &[(&str, &str)] = &[
    ("session-stock", "tip"),
    ("session-stock", "best_header"),
    ("session-stock", "ibd"),
    ("session-stock", "tips"),
    ("session-stock", "sporks"),
    ("session-stock", "reply"),
    ("session-stock", "utxo.txouts"),
    ("session-stock", "utxo.total_amount"),
    ("session-stock", "utxo.bestblock"),
    ("session-stock", "utxo.muhash"),
    ("session-hooked", "tip"),
    ("session-hooked", "best_header"),
    ("session-hooked", "ibd"),
    ("session-hooked", "tips"),
    ("session-hooked", "sporks"),
    ("session-hooked", "reply"),
    ("session-hooked", "utxo.txouts"),
    ("session-hooked", "utxo.total_amount"),
    ("session-hooked", "utxo.bestblock"),
    ("session-hooked", "utxo.muhash"),
    ("session-hooked", "records"),
    ("session-hooked", "attempts"),
    ("session-hooked", "sparse records"),
    ("stock-hooked", "reply"),
    ("stock-hooked", "state"),
    ("stock-hooked", "utxo"),
];

/// Where the session side comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provenance {
    /// rust-dashcore's `HEAD`, and whether its tree had changes.
    pub rdc_head: String,
    pub rdc_dirty: bool,
    /// The linked shim's ABI.
    pub abi: i32,
}

impl Provenance {
    /// This build's: `git` in the crate's directory, and the linked oracle's ABI.
    pub fn here() -> Result<Provenance, String> {
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .output()
                .map_err(|e| format!("git: {e}"))?;
            if !out.status.success() {
                return Err(format!("git {args:?}: {}", out.status));
            }
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
        };
        Ok(Provenance {
            rdc_head: git(&["rev-parse", "HEAD"])?,
            rdc_dirty: !git(&["status", "--porcelain", "--untracked-files=no"])?.is_empty(),
            abi: dash_oracle_sys::Oracle::new(dash_oracle_sys::Pin::V24_0).abi_version(),
        })
    }
}

/// One schedule's counts.
#[derive(Clone, Debug, Default)]
pub struct ScheduleCounts {
    pub schedule: &'static str,
    pub categories: BTreeMap<Category, usize>,
    pub contexts: BTreeMap<Context, usize>,
    /// Programs with at least one executed proposal.
    pub with_proposal: usize,
    pub tally: Tally,
    pub agreement: Agreement,
    pub disagreements: Vec<Disagreement>,
    /// Each binary's dashd arguments on this schedule ([`super::Dashd::arguments`]).
    pub dashd_args: BTreeMap<&'static str, Vec<String>>,
    /// Sampled session `A` records answering `sb_required` true ([`super::sb_required`]): 0 by
    /// construction.
    pub sb_required: u64,
}

/// The whole run's evidence.
#[derive(Clone, Debug)]
pub struct Evidence {
    pub provenance: Provenance,
    pub binaries: Vec<Binary>,
    pub minimums: Minimums,
    pub schedules: Vec<ScheduleCounts>,
    pub spork: Vec<Row>,
}

impl Evidence {
    /// Why the run fails: a binary missing, schedules other than [`M3_SCHEDULES`], a schedule
    /// below its minimums or without an equal comparison in one of [`EXPECTED_ROWS`], an
    /// undisposed disagreement, or the spork experiment's verdict.
    pub fn failures(&self) -> Vec<String> {
        let mut out = Vec::new();
        for kind in [super::Kind::Stock, super::Kind::Hooked] {
            if !self.binaries.iter().any(|b| b.kind == kind) {
                out.push(format!("no {} binary", kind.name()));
            }
        }
        let mut ran: Vec<&str> = self.schedules.iter().map(|s| s.schedule).collect();
        ran.sort_unstable();
        if ran != M3_SCHEDULES {
            out.push(format!(
                "schedules {}: not the M3 gate, which needs {}",
                ran.join(","),
                M3_SCHEDULES.join(",")
            ));
        }
        for s in &self.schedules {
            for (pair, plane) in EXPECTED_ROWS {
                if s.agreement.get(&(*pair, *plane)).is_none_or(|(equal, _)| *equal == 0) {
                    out.push(format!("{}: no equal {pair} {plane} comparison", s.schedule));
                }
            }
            if s.with_proposal < self.minimums.programs {
                out.push(format!(
                    "{}: {} programs with a proposal, below {}",
                    s.schedule, s.with_proposal, self.minimums.programs
                ));
            }
            for c in CONTEXTS {
                let n = s.contexts.get(&c).copied().unwrap_or(0);
                if n < self.minimums.per_context {
                    out.push(format!(
                        "{}: {} {n}, below {}",
                        s.schedule,
                        c.name(),
                        self.minimums.per_context
                    ));
                }
            }
            for d in s.disagreements.iter().filter(|d| d.disposition.is_none()) {
                out.push(format!(
                    "{} {} step {} {} {}: {} differs, undisposed",
                    d.schedule, d.program, d.step, d.op, d.pair, d.plane
                ));
            }
        }
        if let Err(e) = spork::verdict(&self.spork) {
            out.push(format!("the spork experiment: {e}"));
        }
        out
    }

    pub fn to_json(&self) -> Value {
        let binaries: Vec<Value> = self
            .binaries
            .iter()
            .map(|b| {
                json!({
                    "kind": b.kind.name(),
                    "sha256": b.sha256,
                    "version": b.version,
                    "build_info_sha256": b.build_info_sha256,
                    "provenance": b.provenance,
                })
            })
            .collect();
        let schedules: Vec<Value> = self
            .schedules
            .iter()
            .map(|s| {
                json!({
                    "schedule": s.schedule,
                    "categories": s.categories.iter().map(|(c, n)| (c.name(), *n)).collect::<BTreeMap<_, _>>(),
                    "contexts": CONTEXTS.iter().map(|c| (c.name(), s.contexts.get(c).copied().unwrap_or(0))).collect::<BTreeMap<_, _>>(),
                    "with_proposal": s.with_proposal,
                    "draws": s.tally.draws,
                    "tainted": s.tally.tainted,
                    "empty": s.tally.empty,
                    "excluded": s.tally.excluded,
                    "ended": END_KINDS.iter().map(|k| (*k, s.tally.ended.get(k).copied().unwrap_or(0))).collect::<BTreeMap<_, _>>(),
                    "dashd_args": s.dashd_args,
                    "sb_required": s.sb_required,
                    "agreement": s.agreement.iter().map(|((pair, plane), (equal, differ))| json!({
                        "pair": pair, "plane": plane, "equal": equal, "differ": differ,
                    })).collect::<Vec<_>>(),
                    "disagreements": s.disagreements.iter().map(disagreement_json).collect::<Vec<_>>(),
                })
            })
            .collect();
        let spork: Vec<Value> = self
            .spork
            .iter()
            .map(|r| {
                json!({
                    "system": r.system, "trial": r.trial, "check": r.check,
                    "expected": spork_json(Some(&r.expected)), "observed": spork_json(r.observed.as_ref()),
                    "passed": r.passed(),
                })
            })
            .collect();
        json!({
            "evidence": NAME,
            "rdc_head": self.provenance.rdc_head,
            "rdc_dirty": self.provenance.rdc_dirty,
            "abi": self.provenance.abi,
            "binaries": binaries,
            "minimums": {"programs": self.minimums.programs, "per_context": self.minimums.per_context},
            "schedules": schedules,
            "spork": spork,
            "failures": self.failures(),
        })
    }

    pub fn to_markdown(&self) -> String {
        let mut md = String::new();
        let p = &self.provenance;
        let _ = writeln!(md, "# {NAME}\n");
        let _ = writeln!(
            md,
            "- rdc head: `{}`{}",
            p.rdc_head,
            if p.rdc_dirty {
                " (with changes)"
            } else {
                ""
            }
        );
        let _ = writeln!(md, "- shim ABI: {}", p.abi);
        let _ = writeln!(
            md,
            "- minimums per schedule: {} programs with an executed proposal, {} per context\n",
            self.minimums.programs, self.minimums.per_context
        );
        let _ = writeln!(
            md,
            "## Binaries\n\n| kind | sha256 | version | build-info sha256 | provenance |\n|---|---|---|---|---|"
        );
        for b in &self.binaries {
            let _ = writeln!(
                md,
                "| {} | `{}` | {} | {} | {} |",
                b.kind.name(),
                b.sha256,
                b.version,
                b.build_info_sha256.as_deref().map_or("-".into(), |s| format!("`{s}`")),
                if b.provenance.is_empty() {
                    "-".into()
                } else {
                    b.provenance.join("; ")
                }
            );
        }
        for s in &self.schedules {
            let _ = writeln!(md, "\n## {}\n", s.schedule);
            let cats: Vec<String> =
                s.categories.iter().map(|(c, n)| format!("{} {n}", c.name())).collect();
            let _ = writeln!(md, "- categories: {}", cats.join(", "));
            let _ = writeln!(md, "- programs with an executed proposal: {}", s.with_proposal);
            let ctx: Vec<String> = CONTEXTS
                .iter()
                .map(|c| format!("{} {}", c.name(), s.contexts.get(c).copied().unwrap_or(0)))
                .collect();
            let _ = writeln!(md, "- executed proposals per context: {}", ctx.join(", "));
            let excluded: Vec<String> =
                s.tally.excluded.iter().map(|(w, n)| format!("{w} {n}")).collect();
            let ended: Vec<String> = END_KINDS
                .iter()
                .map(|k| format!("{k} {}", s.tally.ended.get(k).copied().unwrap_or(0)))
                .collect();
            let _ = writeln!(
                md,
                "- generated draws {}, empty {}, tie-tainted {}, excluded: {}; sampled runs ended: \
                 {}",
                s.tally.draws,
                s.tally.empty,
                s.tally.tainted,
                if excluded.is_empty() {
                    "none".into()
                } else {
                    excluded.join(", ")
                },
                ended.join(", ")
            );
            let _ = writeln!(
                md,
                "- sampled A records with sb_required true: {} (0 by construction: superblock \
                 validation is never required in the X-check, because no X-check dashd is \
                 mnsync-synced; the superblock payment checks are not covered, with governance on \
                 or off)",
                s.sb_required
            );
            for (kind, args) in &s.dashd_args {
                let _ = writeln!(md, "- {kind} dashd arguments: `{}`", args.join(" "));
            }
            let _ = writeln!(md, "\n| pair | plane | equal | differ |\n|---|---|---|---|");
            for ((pair, plane), (equal, differ)) in &s.agreement {
                let _ = writeln!(md, "| {pair} | {plane} | {equal} | {differ} |");
            }
            if !s.disagreements.is_empty() {
                let _ = writeln!(
                    md,
                    "\n| program | pair | step | op | plane | disposition |\n|---|---|---|---|---|---|"
                );
                for d in &s.disagreements {
                    let _ = writeln!(
                        md,
                        "| {} | {} | {} | {} | {} | {} |",
                        d.program,
                        d.pair,
                        d.step,
                        d.op,
                        d.plane,
                        d.disposition.unwrap_or("**none**")
                    );
                }
            }
        }
        let _ = writeln!(
            md,
            "\n## Spork experiment\n\n| system | trial | check | expected | observed | |\n|---|---|---|---|---|---|"
        );
        for r in &self.spork {
            let _ = writeln!(
                md,
                "| {} | {} | {} | {} | {} | {} |",
                r.system,
                r.trial,
                r.check,
                spork_text(Some(&r.expected)),
                spork_text(r.observed.as_ref()),
                if r.passed() {
                    "pass"
                } else {
                    "FAIL"
                }
            );
        }
        let failures = self.failures();
        let _ = writeln!(md, "\n## Verdict\n");
        if failures.is_empty() {
            let _ = writeln!(md, "PASS");
        } else {
            for f in failures {
                let _ = writeln!(md, "- {f}");
            }
            let _ = writeln!(md, "\nFAIL");
        }
        md
    }
}

fn disagreement_json(d: &Disagreement) -> Value {
    json!({
        "program": d.program, "pair": d.pair, "step": d.step, "op": d.op, "plane": d.plane,
        "disposition": d.disposition,
    })
}

fn spork_json(s: Option<&dash_oracle_sys::SporkState>) -> Value {
    s.map_or(Value::Null, |s| json!({"id": s.0, "value": s.1, "active": s.2}))
}

fn spork_text(s: Option<&dash_oracle_sys::SporkState>) -> String {
    s.map_or("none".into(), |s| {
        format!(
            "{} {}",
            s.1,
            if s.2 {
                "on"
            } else {
                "off"
            }
        )
    })
}

/// Writes both files into `dir` (0700), then fails if the evidence does: the files are kept
/// either way.
pub fn write(evidence: &Evidence, dir: &Path) -> Result<(), String> {
    private_dir(dir)?;
    let json = serde_json::to_string_pretty(&evidence.to_json()).map_err(|e| e.to_string())?;
    for (name, text) in
        [(format!("{NAME}.json"), json + "\n"), (format!("{NAME}.md"), evidence.to_markdown())]
    {
        let path = dir.join(name);
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    let failures = evidence.failures();
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n"))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::collections::BTreeMap;
    use std::os::unix::fs::PermissionsExt;

    use dash_oracle_sys::RestartKind;

    use super::{EXPECTED_ROWS, Evidence, M3_SCHEDULES, NAME, Provenance, ScheduleCounts, write};
    use crate::chain::xcheck::sample::{CONTEXTS, Category, Minimums, Tally};
    use crate::chain::xcheck::spork::{CHECKS, Row, SYSTEMS, TRIALS, expected, trial_name};
    use crate::chain::xcheck::{Binary, Disagreement, Kind};

    fn binary(kind: Kind) -> Binary {
        Binary {
            path: format!("/bin/{}", kind.name()).into(),
            kind,
            sha256: format!("{:064}", kind as u8),
            version: "Dash Core version v24.0.0".into(),
            build_info_sha256: (kind == Kind::Hooked).then(|| "b".repeat(64)),
            provenance: if kind == Kind::Hooked {
                vec!["commit 586c0ebb45df".into()]
            } else {
                Vec::new()
            },
        }
    }

    fn evidence() -> Evidence {
        let mut spork = Vec::new();
        for system in SYSTEMS {
            for trial in TRIALS {
                for check in CHECKS {
                    spork.push(Row {
                        system,
                        trial: trial_name(trial),
                        check,
                        expected: expected(trial, check),
                        observed: Some(expected(trial, check)),
                    });
                }
            }
        }
        let minimums = Minimums {
            programs: 2,
            per_context: 1,
        };
        let schedules = M3_SCHEDULES
            .map(|schedule| ScheduleCounts {
                schedule,
                categories: BTreeMap::from([(Category::Seed, 3), (Category::Generated, 4)]),
                contexts: CONTEXTS.iter().map(|c| (*c, 1)).collect(),
                with_proposal: 2,
                agreement: EXPECTED_ROWS.iter().map(|row| (*row, (9, 0))).collect(),
                dashd_args: BTreeMap::from([(
                    "stock",
                    vec!["-regtest".into(), "-disablegovernance=1".into()],
                )]),
                sb_required: 0,
                tally: Tally {
                    ended: BTreeMap::from([("done", 5), ("diverged", 2)]),
                    ..Tally::default()
                },
                ..ScheduleCounts::default()
            })
            .into();
        Evidence {
            provenance: Provenance {
                rdc_head: "a".repeat(40),
                rdc_dirty: false,
                abi: 36,
            },
            binaries: vec![binary(Kind::Stock), binary(Kind::Hooked)],
            minimums,
            schedules,
            spork,
        }
    }

    /// 4f test 7: the evidence names both binaries with their provenance, counts by category and
    /// context, holds the agreement table and the twelve spork rows; a disagreement without a
    /// disposition, a missing spork row, a schedule below its minimums, a missing binary, a
    /// schedule of M3's not run, or an expected row never compared equal fails the writer,
    /// which still writes both files.
    #[test]
    fn the_evidence_names_both_binaries_and_counts_by_category() {
        let dir = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let out = dir.path().join("evidence");
        let e = evidence();
        write(&e, &out).unwrap();
        assert_eq!(std::fs::metadata(&out).unwrap().permissions().mode() & 0o777, 0o700);
        let json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(out.join(format!("{NAME}.json"))).unwrap())
                .unwrap();
        assert_eq!(json["evidence"], NAME);
        assert_eq!(json["rdc_head"], "a".repeat(40));
        assert_eq!(json["abi"], 36);
        let kinds: Vec<&str> = json["binaries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["kind"].as_str().unwrap())
            .collect();
        assert_eq!(kinds, ["stock", "hooked"]);
        assert_eq!(json["binaries"][1]["build_info_sha256"], "b".repeat(64));
        assert_eq!(json["binaries"][1]["provenance"][0], "commit 586c0ebb45df");
        assert_eq!(json["binaries"][0]["sha256"].as_str().unwrap().len(), 64);
        let s0 = &json["schedules"][0];
        assert_eq!(
            (s0["categories"]["seed"].as_u64(), s0["categories"]["generated"].as_u64()),
            (Some(3), Some(4))
        );
        assert_eq!(s0["contexts"].as_object().unwrap().len(), 7);
        assert_eq!(s0["agreement"][0]["equal"], 9);
        assert_eq!(s0["dashd_args"]["stock"][1], "-disablegovernance=1");
        assert_eq!(s0["sb_required"], 0);
        assert_eq!(
            s0["ended"],
            serde_json::json!({"done": 5, "unexpected": 0, "diverged": 2, "unsupported": 0})
        );
        assert_eq!(json["spork"].as_array().unwrap().len(), 12);
        assert_eq!(json["failures"].as_array().unwrap().len(), 0);
        let md = std::fs::read_to_string(out.join(format!("{NAME}.md"))).unwrap();
        assert!(
            md.contains("| hooked |") && md.contains("| stock |") && md.ends_with("PASS\n"),
            "{md}"
        );
        assert!(
            md.contains("; sampled runs ended: done 5, unexpected 0, diverged 2, unsupported 0\n"),
            "{md}"
        );

        let mut undisposed = evidence();
        undisposed.schedules[0].disagreements.push(Disagreement {
            schedule: "S0",
            program: "basic".into(),
            pair: "stock-hooked",
            step: "3".into(),
            op: "block",
            plane: "reply",
            disposition: None,
            detail: "x".into(),
        });
        let again = dir.path().join("undisposed");
        let err = write(&undisposed, &again).unwrap_err();
        assert!(
            err.contains("basic step 3 block stock-hooked: reply differs, undisposed"),
            "{err}"
        );
        assert!(
            again.join(format!("{NAME}.json")).exists()
                && again.join(format!("{NAME}.md")).exists()
        );
        let md = std::fs::read_to_string(again.join(format!("{NAME}.md"))).unwrap();
        assert!(md.contains("**none**") && md.ends_with("FAIL\n"));
        let text = std::fs::read_to_string(again.join(format!("{NAME}.json"))).unwrap();
        assert!(
            !text.contains("detail") && !md.contains("detail"),
            "a disagreement's detail stays out"
        );

        let mut skipped = evidence();
        skipped
            .spork
            .retain(|r| !(r.system == "dashd" && r.trial == trial_name(RestartKind::Kill)));
        assert!(write(&skipped, &dir.path().join("skipped")).unwrap_err().contains("missing"));
        let mut short = evidence();
        short.schedules[0].with_proposal = 1;
        assert!(write(&short, &dir.path().join("short")).unwrap_err().contains("below 2"));
        let mut lone = evidence();
        lone.binaries.pop();
        assert!(write(&lone, &dir.path().join("lone")).unwrap_err().contains("no hooked binary"));
        let mut narrow = evidence();
        narrow.schedules.truncate(1);
        let err = write(&narrow, &dir.path().join("narrow")).unwrap_err();
        assert!(err.contains("schedules S0: not the M3 gate, which needs S0,S1,S2"), "{err}");
        let mut unrun = evidence();
        unrun.schedules[2].agreement.remove(&("session-hooked", "attempts"));
        unrun.schedules[1].agreement.insert(("stock-hooked", "utxo"), (0, 0));
        let err = write(&unrun, &dir.path().join("unrun")).unwrap_err();
        assert!(err.contains("S2: no equal session-hooked attempts comparison"), "{err}");
        assert!(err.contains("S1: no equal stock-hooked utxo comparison"), "{err}");
        assert_eq!(err.lines().count(), 2, "{err}");
    }
}
