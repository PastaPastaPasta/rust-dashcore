//! Per-PR window selection (O9b S4; R4 §2.3.4, A1 §4.6; DEC-697 Q8): which segment windows a
//! change runs, from the paths it touches, under the per-PR budget.
//!
//! The registry (`conformance/segments.json`) lists path rules and windows. A path that matches
//! a rule's prefix hits that rule's areas, `*` being every area. A window runs when it is active
//! and one of its areas is hit. The selected windows are taken first-fit in registry order under
//! the budget (55 min): a window over what is left goes to the nightly, which runs every active
//! window, and a later one that fits still runs. A deferred window (phase B, or data not yet cleared)
//! never runs, and is listed when its areas are hit. A change that hits no rule runs nothing.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context as _, Result, ensure};
use serde::Deserialize;

/// The registry's `format`.
pub const FORMAT: &str = "O9b segment registry v1";
/// The area every area matches.
const ALL: &str = "*";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    pub format: String,
    /// The per-PR budget, in seconds of wall time.
    pub budget_s: u64,
    /// A window's wall time until one is measured.
    pub default_wall_s: u64,
    pub rules: Vec<Rule>,
    pub windows: Vec<Window>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub prefix: String,
    pub areas: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Window {
    pub id: String,
    /// `main` or `test`, as `digest l2 --chain`.
    pub chain: String,
    /// The bundle's base B: the window is B+1..=`to`.
    pub base: u32,
    pub to: u32,
    pub transition: String,
    pub areas: Vec<String>,
    /// The last measured wall time of the whole window run, in seconds.
    pub wall_s: Option<u64>,
    /// Why the window does not run yet; absent when it is active.
    pub deferred: Option<String>,
}

impl Registry {
    /// Reads and checks the registry at `path`.
    pub fn load(path: &Path) -> Result<Registry> {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let r: Registry = serde_json::from_slice(&bytes)
            .with_context(|| format!("{}: not a segment registry", path.display()))?;
        r.check().with_context(|| format!("{}", path.display()))?;
        Ok(r)
    }

    fn check(&self) -> Result<()> {
        ensure!(self.format == FORMAT, "not a {FORMAT}");
        let mut ids = BTreeSet::new();
        let known: BTreeSet<&str> =
            self.windows.iter().flat_map(|w| w.areas.iter().map(String::as_str)).collect();
        for rule in &self.rules {
            ensure!(!rule.prefix.is_empty() && !rule.areas.is_empty(), "an empty rule");
            for area in &rule.areas {
                ensure!(
                    area == ALL || known.contains(area.as_str()),
                    "rule {}: no window has area {area}",
                    rule.prefix
                );
            }
        }
        for w in &self.windows {
            ensure!(ids.insert(&w.id), "window {} is listed twice", w.id);
            ensure!(w.chain == "main" || w.chain == "test", "window {}: chain {}", w.id, w.chain);
            ensure!(w.to > w.base, "window {}: ends at or below its base", w.id);
            ensure!(
                !w.areas.is_empty() && !w.areas.iter().any(|a| a == ALL),
                "window {}: areas",
                w.id
            );
        }
        Ok(())
    }

    fn wall(&self, w: &Window) -> u64 {
        w.wall_s.unwrap_or(self.default_wall_s)
    }
}

/// What a selection decided.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Selection {
    /// The windows to run now, in order.
    pub run: Vec<Window>,
    /// Selected, but over the budget: the nightly runs them.
    pub nightly: Vec<Window>,
    /// Hit, but deferred.
    pub deferred: Vec<Window>,
    /// The summed wall time of `run`.
    pub wall_s: u64,
}

/// The areas `paths` (repo-relative, a leading `./` allowed) hit.
pub fn areas_hit<'a>(
    registry: &Registry,
    paths: impl IntoIterator<Item = &'a str>,
) -> BTreeSet<String> {
    let mut hit = BTreeSet::new();
    for path in paths {
        let path = path.trim_start_matches("./");
        for rule in registry.rules.iter().filter(|r| path.starts_with(&r.prefix)) {
            hit.extend(rule.areas.iter().cloned());
        }
    }
    hit
}

/// The per-PR selection for `paths`; with `nightly`, every active window, with no budget.
pub fn select<'a>(
    registry: &Registry,
    paths: impl IntoIterator<Item = &'a str>,
    nightly: bool,
) -> Selection {
    let hit = areas_hit(registry, paths);
    let wanted =
        |w: &Window| nightly || hit.contains(ALL) || w.areas.iter().any(|a| hit.contains(a));
    let mut out = Selection::default();
    for w in registry.windows.iter().filter(|w| wanted(w)) {
        let wall = registry.wall(w);
        if w.deferred.is_some() {
            out.deferred.push(w.clone());
        } else if nightly || out.wall_s + wall <= registry.budget_s {
            out.wall_s += wall;
            out.run.push(w.clone());
        } else {
            out.nightly.push(w.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(id: &str, areas: &[&str], wall: Option<u64>, deferred: bool) -> Window {
        Window {
            id: id.to_owned(),
            chain: "main".to_owned(),
            base: 10,
            to: 20,
            transition: "t".to_owned(),
            areas: areas.iter().map(|a| (*a).to_owned()).collect(),
            wall_s: wall,
            deferred: deferred.then(|| "phase B".to_owned()),
        }
    }

    fn registry() -> Registry {
        let rule = |prefix: &str, areas: &[&str]| Rule {
            prefix: prefix.to_owned(),
            areas: areas.iter().map(|a| (*a).to_owned()).collect(),
        };
        Registry {
            format: FORMAT.to_owned(),
            budget_s: 1000,
            default_wall_s: 400,
            rules: vec![
                rule("dash-consensus/", &["*"]),
                rule("dash-evo/", &["evo"]),
                rule("dash-governance/", &["payments", "evo"]),
            ],
            windows: vec![
                window("pow", &["pow"], Some(300), false),
                window("pay", &["payments"], None, false),
                window("pay2", &["payments"], Some(500), false),
                window("evo", &["evo"], None, true),
            ],
        }
    }

    fn ids(ws: &[Window]) -> Vec<&str> {
        ws.iter().map(|w| w.id.as_str()).collect()
    }

    #[test]
    fn the_shipped_registry_loads() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("segments.json");
        let r = Registry::load(&path).unwrap();
        let all = select(&r, ["dash-consensus/src/lib.rs"], false);
        assert_eq!(ids(&all.run), ["main-68589", "main-328007", "main-614819"]);
        assert!(all.nightly.is_empty(), "every phase A window fits the per-PR budget");
        assert_eq!(select(&r, ["./dash-kv/src/lib.rs"], false).run, all.run);
        assert_eq!(select(&r, ["README.md"], false), Selection::default());
    }

    #[test]
    fn a_path_outside_every_rule_runs_nothing() {
        assert_eq!(
            select(&registry(), ["key-wallet/src/lib.rs", "docs/x.md"], false),
            Selection::default()
        );
    }

    #[test]
    fn areas_select_their_windows_and_deferred_ones_never_run() {
        let r = registry();
        let s = select(&r, ["dash-governance/src/a.rs"], false);
        assert_eq!(ids(&s.run), ["pay", "pay2"]);
        assert_eq!(s.wall_s, 900);
        assert_eq!(ids(&s.deferred), ["evo"]);
        let s = select(&r, ["dash-evo/src/a.rs"], false);
        assert!(s.run.is_empty());
        assert_eq!(ids(&s.deferred), ["evo"]);
    }

    #[test]
    fn the_budget_sends_the_rest_to_the_nightly() {
        let r = registry();
        // 300 + 400 fits 1000; 500 more does not.
        let s = select(&r, ["dash-consensus/src/pow.rs"], false);
        assert_eq!(ids(&s.run), ["pow", "pay"]);
        assert_eq!(ids(&s.nightly), ["pay2"]);
        assert_eq!(s.wall_s, 700);
        let s = select(&r, [], true);
        assert_eq!(ids(&s.run), ["pow", "pay", "pay2"]);
        assert!(s.nightly.is_empty());

        // First-fit: a later window that fits after one that did not still runs.
        let mut r = registry();
        r.windows.push(window("small", &["pow"], Some(250), false));
        let s = select(&r, ["dash-consensus/src/pow.rs"], false);
        assert_eq!(ids(&s.run), ["pow", "pay", "small"]);
        assert_eq!(ids(&s.nightly), ["pay2"]);
        assert_eq!(s.wall_s, 950);
    }

    #[test]
    fn a_bad_registry_is_refused() {
        let mut r = registry();
        r.windows.push(window("pow", &["pow"], None, false));
        assert!(r.check().unwrap_err().to_string().contains("twice"));
        let mut r = registry();
        r.rules.push(Rule {
            prefix: "x/".to_owned(),
            areas: vec!["nope".to_owned()],
        });
        assert!(r.check().unwrap_err().to_string().contains("no window has area nope"));
        let mut r = registry();
        r.windows[0].to = r.windows[0].base;
        assert!(r.check().is_err());
    }
}
