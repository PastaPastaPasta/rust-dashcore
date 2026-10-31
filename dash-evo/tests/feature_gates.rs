//! The two dash-evo features that build a `Verified` quorum without verifying it are each
//! enabled by one crate only: `reload` by dash-mnlist (DEC-412), `mined` by dashd-chainstate
//! (OD-CP-2). A crate that depends on the owner gets the feature by Cargo's unification, as
//! node-l1's harness does: that is the owner's build, not a second owner.
//!
//! Every `Cargo.toml` of the repository is parsed as TOML. Each one with a `workspace` table (in
//! any TOML form) is a workspace root and is read through `cargo metadata --no-deps`, which gives
//! each package's declared dependency features and feature tables whatever the manifest syntax
//! (inline or dotted tables, multi-line arrays, renamed dependencies, `dep?/feature`). A manifest
//! with a `package` table that no root's read returns (a package a workspace excludes, which is
//! then its own root) is read through its own manifest, and must be among what that read returns,
//! so no package escapes the check. dash-evo's own feature table may not turn either feature on either: a
//! default or another feature implying it would hand it to every dependent.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

/// The feature, and the one package allowed to enable it.
const GATED: [(&str, &str); 2] = [("reload", "dash-mnlist"), ("mined", "dashd-chainstate")];

/// Every `Cargo.toml` under `dir`. A directory with its own `.git` is another checkout, not this
/// tree.
fn manifests(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("a readable directory") {
        let path = entry.expect("an entry").path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if path.is_dir() {
            if name != "target" && !name.starts_with('.') && !path.join(".git").exists() {
                manifests(&path, out);
            }
        } else if name == "Cargo.toml" {
            out.push(path);
        }
    }
}

fn metadata(manifest: &Path) -> Value {
    // The cargo that builds this test, which Cargo names at compile time.
    let out = Command::new(env!("CARGO"))
        .args(["metadata", "--no-deps", "--offline", "--format-version", "1", "--manifest-path"])
        .arg(manifest)
        .output()
        .expect("cargo runs");
    assert!(
        out.status.success(),
        "{}: {}",
        manifest.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("cargo's JSON")
}

/// Whether `package` turns on dash-evo's `feature`: in a dependency's declared features, in a
/// feature-table value naming the dependency (`dep/f`, `dep?/f`, by its name or its rename), or,
/// for dash-evo itself, anywhere in its own feature table.
fn enables(package: &Value, feature: &str) -> bool {
    let strs = |v: &Value| -> Vec<String> {
        v.as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect()
    };
    let features = package["features"].as_object().cloned().unwrap_or_default();
    if package["name"] == "dash-evo" {
        return features.values().flat_map(strs).any(|v| v == feature);
    }
    let deps: Vec<&Value> = package["dependencies"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|d| d["name"] == "dash-evo")
        .collect();
    if deps.iter().any(|d| strs(&d["features"]).iter().any(|f| f == feature)) {
        return true;
    }
    let keys: Vec<String> = deps
        .iter()
        .map(|d| d["rename"].as_str().unwrap_or("dash-evo").to_owned())
        .chain(["dash-evo".to_owned()])
        .collect();
    features.values().flat_map(strs).any(|v| {
        keys.iter().any(|k| v == format!("{k}/{feature}") || v == format!("{k}?/{feature}"))
    })
}

/// A manifest as TOML, or the error that names it.
fn table(manifest: &Path) -> Result<toml::Table, String> {
    let text =
        std::fs::read_to_string(manifest).map_err(|e| format!("{}: {e}", manifest.display()))?;
    text.parse().map_err(|e| format!("{}: {e}", manifest.display()))
}

/// Per gated feature, the packages under `root` that enable it, sorted.
fn owners(root: &Path) -> Vec<(&'static str, Vec<String>)> {
    let mut all = Vec::new();
    manifests(root, &mut all);
    let mut packages = Vec::new();
    let mut packaged = Vec::new();
    for manifest in &all {
        let table = table(manifest).unwrap_or_else(|e| panic!("{e}"));
        if table.contains_key("package") {
            packaged.push(manifest.canonicalize().expect("a path"));
        }
        if table.contains_key("workspace") {
            packages.extend(metadata(manifest)["packages"].as_array().cloned().unwrap_or_default());
        }
    }
    let read = |packages: &[Value]| -> BTreeSet<PathBuf> {
        packages
            .iter()
            .filter_map(|p| p["manifest_path"].as_str())
            .map(|p| Path::new(p).canonicalize().expect("a path"))
            .collect()
    };
    let unread: Vec<PathBuf> =
        packaged.iter().filter(|m| !read(&packages).contains(*m)).cloned().collect();
    for manifest in &unread {
        if !read(&packages).contains(manifest) {
            packages.extend(metadata(manifest)["packages"].as_array().cloned().unwrap_or_default());
        }
        assert!(read(&packages).contains(manifest), "{} is in no read", manifest.display());
    }
    GATED
        .iter()
        .map(|&(feature, _)| {
            let enabling: BTreeSet<String> = packages
                .iter()
                .filter(|p| enables(p, feature))
                .filter_map(|p| p["name"].as_str().map(str::to_owned))
                .collect();
            (feature, enabling.into_iter().collect())
        })
        .collect()
}

/// `Ok` iff each gated feature is enabled by its owner alone.
fn check(owners: &[(&str, Vec<String>)]) -> Result<(), String> {
    for ((feature, enabling), (_, owner)) in owners.iter().zip(GATED) {
        if enabling.as_slice() != [owner] {
            return Err(format!("dash-evo's `{feature}` is enabled by {enabling:?}"));
        }
    }
    Ok(())
}

#[test]
fn unverified_quorum_features_have_one_owner_each() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("the repository");
    assert_eq!(check(&owners(root)), Ok(()));
}

/// Whether a probe may live under `dir`: no directory at or above its canonical path (the path
/// `owners` hands Cargo) holds a manifest with a `workspace` table, or one that is not TOML.
/// Cargo looks past a workspace that excludes a package for one further up, so under the
/// repository (as in its default `target/tmp`) the headerless `second` would be refused as
/// outside that workspace. A `[package]`-only manifest above is no workspace and does no harm.
fn isolated(dir: &Path) -> bool {
    let Ok(dir) = dir.canonicalize() else {
        return false;
    };
    dir.ancestors()
        .map(|a| a.join("Cargo.toml"))
        .filter(|m| m.is_file())
        .all(|m| table(&m).is_ok_and(|t| !t.contains_key("workspace")))
}

/// The first isolated base, canonical: the ambient temporary directory (TMPDIR), else `/tmp`.
fn probe_base() -> PathBuf {
    let candidates = [std::env::temp_dir(), PathBuf::from("/tmp")];
    candidates
        .iter()
        .find(|c| isolated(c))
        .map(|c| c.canonicalize().expect("a path"))
        .unwrap_or_else(|| {
            panic!("no temporary directory outside a Cargo workspace: {candidates:?}")
        })
}

/// A tree with dash-evo, the two owners, and optionally `second`, a crate the root workspace
/// excludes that enables both features, ending with `header` (its own `workspace` table, or
/// nothing: then it is its own root), in a temporary directory under [`probe_base`] removed on
/// drop.
fn probe(header: Option<&str>) -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix("feature-gates")
        .tempdir_in(probe_base())
        .expect("a temporary directory");
    let root = dir.path();
    let package = |dir: &str, rest: &str| {
        std::fs::create_dir_all(root.join(dir).join("src")).expect("a probe dir");
        std::fs::write(root.join(dir).join("src/lib.rs"), "").expect("a probe lib");
        let manifest =
            format!("[package]\nname = \"{dir}\"\nversion = \"0.0.0\"\nedition = \"2021\"\n{rest}");
        std::fs::write(root.join(dir).join("Cargo.toml"), manifest).expect("a probe manifest");
    };
    let dep = |features: &str| {
        format!(
            "\n[dependencies]\ndash-evo = {{ path = \"../dash-evo\", features = [{features}] }}\n"
        )
    };
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"dash-evo\", \"dash-mnlist\", \"dashd-chainstate\"]\n\
         exclude = [\"second\"]\nresolver = \"2\"\n",
    )
    .expect("a probe workspace");
    package("dash-evo", "\n[features]\nreload = []\nmined = []\n");
    package("dash-mnlist", &dep("\"reload\""));
    package("dashd-chainstate", &dep("\"mined\""));
    if let Some(header) = header {
        package("second", &format!("{}\n{header}\n", dep("\"reload\", \"mined\"")));
    }
    dir
}

/// The guard end to end: a second owner in a workspace of its own fails it for both features,
/// whether that workspace's header is plain or carries a comment (Sol r1 m1) or the package is an
/// excluded one without a `workspace` table; the owners alone pass.
#[test]
fn a_second_owner_in_its_own_workspace_fails_the_guard() {
    assert_eq!(check(&owners(probe(None).path())), Ok(()));
    let headers = [("plain", "[workspace]"), ("commented", "[workspace] # a tool"), ("none", "")];
    for (name, header) in headers {
        let found = owners(probe(Some(header)).path());
        let both = ["dash-mnlist".to_owned(), "second".to_owned()];
        assert_eq!(found[0], ("reload", both.to_vec()), "{name}");
        let both = ["dashd-chainstate".to_owned(), "second".to_owned()];
        assert_eq!(found[1], ("mined", both.to_vec()), "{name}");
        assert!(check(&found).is_err(), "{name}");
    }
}

/// A base is judged by its canonical ancestry and TOML: the repository and a symlink into it are
/// not isolated, a directory under a `[package]`-only manifest is, and one under a commented
/// `[workspace]` header is not (Sol r3).
#[test]
fn a_probe_base_is_judged_by_its_canonical_workspaces() {
    assert!(!isolated(Path::new(env!("CARGO_MANIFEST_DIR"))));
    let dir = tempfile::tempdir_in(probe_base()).expect("a temporary directory");
    let under = |name: &str, manifest: &str| {
        std::fs::create_dir_all(dir.path().join(name).join("tmp")).expect("a dir");
        std::fs::write(dir.path().join(name).join("Cargo.toml"), manifest).expect("a manifest");
        isolated(&dir.path().join(name).join("tmp"))
    };
    assert!(under("package", "[package]\nname = \"p\"\nversion = \"0.0.0\"\n"));
    assert!(!under("workspace", "[workspace] # a tool\n"));
    assert!(!under("not-toml", "[workspace\n"));
    #[cfg(unix)]
    {
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(env!("CARGO_MANIFEST_DIR"), &link).expect("a symlink");
        assert!(!isolated(&link));
    }
}

#[test]
fn the_reader_sees_each_form() {
    let pkg = |json: &str| serde_json::from_str::<Value>(json).expect("json");
    let dep = |features: &str, rename: &str| {
        pkg(&format!(
            r#"{{"name": "x", "features": {{}}, "dependencies": [
                {{"name": "dash-evo", "rename": {rename}, "features": {features}}}]}}"#
        ))
    };
    assert!(enables(&dep(r#"["mined"]"#, "null"), "mined"));
    assert!(!enables(&dep(r#"["reload"]"#, "null"), "mined"));
    let table = |value: &str, rename: &str| {
        pkg(&format!(
            r#"{{"name": "x", "features": {{"f": ["{value}"]}}, "dependencies": [
                {{"name": "dash-evo", "rename": {rename}, "features": []}}]}}"#
        ))
    };
    assert!(enables(&table("dash-evo/mined", "null"), "mined"));
    assert!(enables(&table("dash-evo?/mined", "null"), "mined"));
    assert!(enables(&table("evo/mined", r#""evo""#), "mined"));
    assert!(!enables(&table("other/mined", "null"), "mined"));
    let own = |value: &str| {
        pkg(&format!(
            r#"{{"name": "dash-evo", "features": {{"mined": [], "default": ["{value}"]}},
                "dependencies": []}}"#
        ))
    };
    assert!(enables(&own("mined"), "mined"), "a default implying it");
    assert!(!enables(&own("test-utils"), "mined"));
}
