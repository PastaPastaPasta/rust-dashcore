//! The dashd L3 runs against (plan §3.1): the environment's `DASHVERSION`, checked against the
//! v24 one the workflows run; its binary, whose `-version` line must name it; and the commit of
//! its tag in the Dash Core tree. No pin file: the workflows are the pin.

use std::path::{Path, PathBuf};
use std::process::Command;

use bitcoin_hashes::sha256;

/// The workflows whose v24 `DASHVERSION` the run's must equal, relative to the repository root.
pub const WORKFLOWS: [&str; 2] =
    [".github/workflows/node-gate.yml", ".github/workflows/build-and-test.yml"];

/// `^24\.0\.0(-rc\.[0-9]+)?$`.
pub fn check_dashversion(version: &str) -> Result<(), String> {
    let rc = version.strip_prefix("24.0.0").ok_or_else(|| not_v24(version))?;
    match rc.strip_prefix("-rc.") {
        None if rc.is_empty() => Ok(()),
        Some(n) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => Ok(()),
        _ => Err(not_v24(version)),
    }
}

fn not_v24(version: &str) -> String {
    format!("DASHVERSION={version} is not 24.0.0[-rc.N]")
}

/// The one v24 `DASHVERSION` a workflow sets (`DASHVERSION: 24…`, quoted or not). None, or two
/// different ones, is an error.
pub fn workflow_v24(name: &str, yaml: &str) -> Result<String, String> {
    let mut found: Vec<String> = yaml
        .lines()
        .filter_map(|line| line.trim_start().strip_prefix("DASHVERSION:"))
        .map(|value| value.trim().trim_matches('"').to_owned())
        .filter(|value| value.starts_with("24."))
        .collect();
    found.sort();
    found.dedup();
    match found.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(format!("{name} sets no v24 DASHVERSION")),
        many => Err(format!("{name} sets more than one v24 DASHVERSION: {many:?}")),
    }
}

/// The run's `DASHVERSION`: `env`, which must be set, be a v24 version and equal each workflow's
/// v24 one (`(name, yaml)` pairs).
pub fn dashversion(env: Option<&str>, workflows: &[(&str, &str)]) -> Result<String, String> {
    let version = env.ok_or(
        "DASHVERSION is unset (the L3 lane never runs on setup-dashd.py's default)".to_owned(),
    )?;
    check_dashversion(version)?;
    for (name, yaml) in workflows {
        let theirs = workflow_v24(name, yaml)?;
        if theirs != version {
            return Err(format!("DASHVERSION={version} but {name} runs {theirs}"));
        }
    }
    Ok(version.to_owned())
}

/// The binary: `l3_dashd` if set, else exactly `<home>/.rust-dashcore-test/dashcore-<version>/
/// bin/dashd`, where `setup-dashd.py` puts it. No glob.
pub fn binary(version: &str, l3_dashd: Option<&Path>, home: &Path) -> PathBuf {
    l3_dashd.map_or_else(
        || {
            home.join(".rust-dashcore-test")
                .join(format!("dashcore-{version}"))
                .join("bin")
                .join("dashd")
        },
        Path::to_owned,
    )
}

/// The first line of `dashd -version` for `version`. Captured 2026-10-31 from the cached
/// `dashcore-24.0.0-rc.{3,4,5}` binaries setup-dashd.py fetched (`-datadir=<0700 dir> -version`):
/// `Dash Core version v24.0.0-rc.4`, and likewise for rc.3 and rc.5.
pub fn version_line(version: &str) -> String {
    format!("Dash Core version v{version}")
}

/// `binary -version`'s first line, run on `datadir` (a private directory: the probe writes
/// `settings.json` there, DEC-569). It must be [`version_line`]'s.
pub fn probe(binary: &Path, datadir: &Path, version: &str) -> Result<String, String> {
    let out = Command::new(binary)
        .arg(format!("-datadir={}", datadir.display()))
        .arg("-version")
        .output()
        .map_err(|e| format!("{}: {e}", binary.display()))?;
    if !out.status.success() {
        return Err(format!("{} -version: {}", binary.display(), out.status));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout.lines().next().unwrap_or_default().to_owned();
    if line != version_line(version) {
        return Err(format!(
            "{} -version says {line:?}, not {:?}",
            binary.display(),
            version_line(version)
        ));
    }
    Ok(line)
}

/// The commit tag `v<version>` names in the Dash Core tree at `tree`.
pub fn tag_commit(tree: &Path, version: &str) -> Result<String, String> {
    let tag = format!("v{version}^{{commit}}");
    let out = Command::new("git")
        .arg("-C")
        .arg(tree)
        .args(["rev-parse", "--verify", "--quiet", &tag])
        .output()
        .map_err(|e| format!("git -C {}: {e}", tree.display()))?;
    let commit = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if !out.status.success() || commit.len() != 40 {
        return Err(format!("tag v{version} does not resolve in {}", tree.display()));
    }
    Ok(commit)
}

/// The hex sha256 of the file at `path`.
pub fn file_sha256(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(super::trial::to_hex(&sha256::Hash::hash(&bytes).to_byte_array()))
}

/// The dashd a run uses, as its report records it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dashd {
    pub version: String,
    pub binary: PathBuf,
    pub line: String,
    pub sha256: String,
    pub tag: String,
    pub commit: String,
}

/// Every check of §3.1 over the process's environment: `DASHVERSION` (against the workflows
/// under `repo`), `L3_DASHD` or the cached binary under `HOME`, its `-version` line (probed on a
/// new directory under `scratch`), and `L3_DASH_TREE`'s tag. Any failure is a harness error.
pub fn resolve(repo: &Path, scratch: &Path) -> Result<Dashd, String> {
    let mut workflows = Vec::new();
    for name in WORKFLOWS {
        let path = repo.join(name);
        let yaml =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        workflows.push((name, yaml));
    }
    let pairs: Vec<(&str, &str)> = workflows.iter().map(|(n, y)| (*n, y.as_str())).collect();
    let env = std::env::var("DASHVERSION").ok();
    let version = dashversion(env.as_deref(), &pairs)?;
    let home = std::env::var_os("HOME").ok_or("HOME is unset")?;
    let l3_dashd = std::env::var_os("L3_DASHD").map(PathBuf::from);
    let binary = binary(&version, l3_dashd.as_deref(), Path::new(&home));
    if !binary.is_file() {
        return Err(format!("no dashd at {}", binary.display()));
    }
    let datadir = private_dir(scratch, "version-probe")?;
    let line = probe(&binary, &datadir, &version)?;
    let tree = std::env::var_os("L3_DASH_TREE")
        .ok_or("L3_DASH_TREE is unset (the Dash Core tree whose tag pins the source)")?;
    let commit = tag_commit(Path::new(&tree), &version)?;
    Ok(Dashd {
        sha256: file_sha256(&binary)?,
        tag: format!("v{version}"),
        version,
        binary,
        line,
        commit,
    })
}

/// A new 0700 directory `name` under `parent`; an existing one only if it is a 0700 directory.
pub fn private_dir(parent: &Path, name: &str) -> Result<PathBuf, String> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let dir = parent.join(name);
    match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let meta =
                std::fs::symlink_metadata(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            if !meta.is_dir() || meta.permissions().mode() & 0o777 != 0o700 {
                return Err(format!(
                    "{}: exists, and is not a 0700 directory ({:o})",
                    dir.display(),
                    meta.permissions().mode()
                ));
            }
        }
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
    }

    #[test]
    fn only_v24_versions_pass() {
        for ok in ["24.0.0", "24.0.0-rc.4", "24.0.0-rc.12"] {
            check_dashversion(ok).expect(ok);
        }
        for bad in ["", "23.1.0", "24.0.1", "24.0.0-rc.", "24.0.0-rc.4a", "24.0.0rc.4", " 24.0.0"] {
            assert!(check_dashversion(bad).is_err(), "{bad:?}");
        }
    }

    /// Both workflows in the tree set one v24 DASHVERSION, the same one, and it passes.
    #[test]
    fn the_workflows_agree_on_v24() {
        let mut seen = Vec::new();
        for name in WORKFLOWS {
            let yaml = std::fs::read_to_string(repo().join(name)).expect("the workflow");
            let v = workflow_v24(name, &yaml).expect("one v24 DASHVERSION");
            check_dashversion(&v).expect("a v24 version");
            seen.push(v);
        }
        assert_eq!(seen[0], seen[1]);
    }

    #[test]
    fn a_workflow_needs_exactly_one_v24_version() {
        let one = "env:\n  DASHVERSION: 23.1.0\n  steps:\n    DASHVERSION: \"24.0.0-rc.4\"\n";
        assert_eq!(workflow_v24("w", one), Ok("24.0.0-rc.4".to_owned()));
        let same_twice = format!("{one}    DASHVERSION: 24.0.0-rc.4\n");
        assert_eq!(workflow_v24("w", &same_twice), Ok("24.0.0-rc.4".to_owned()));
        let two = format!("{one}    DASHVERSION: 24.0.0-rc.5\n");
        assert!(workflow_v24("w", &two).is_err_and(|e| e.contains("more than one")));
        assert!(workflow_v24("w", "DASHVERSION: 23.1.0\n").is_err_and(|e| e.contains("no v24")));
    }

    #[test]
    fn the_run_s_version_is_set_well_formed_and_the_workflows() {
        let a = ("a.yml", "DASHVERSION: 24.0.0-rc.4\n");
        let b = ("b.yml", "DASHVERSION: \"24.0.0-rc.4\"\n");
        assert_eq!(dashversion(Some("24.0.0-rc.4"), &[a, b]), Ok("24.0.0-rc.4".to_owned()));
        assert!(dashversion(None, &[a, b]).is_err_and(|e| e.contains("unset")));
        assert!(dashversion(Some("23.1.0"), &[a, b]).is_err_and(|e| e.contains("not 24.0.0")));
        let e = dashversion(Some("24.0.0-rc.5"), &[a, b]).expect_err("disagrees");
        assert_eq!(e, "DASHVERSION=24.0.0-rc.5 but a.yml runs 24.0.0-rc.4");
        let c = ("c.yml", "DASHVERSION: 24.0.0-rc.3\n");
        assert!(dashversion(Some("24.0.0-rc.4"), &[a, c]).is_err_and(|e| e.contains("c.yml")));
    }

    #[test]
    fn the_binary_is_l3_dashd_or_the_cached_one() {
        let home = Path::new("/home/x");
        assert_eq!(
            binary("24.0.0-rc.4", None, home),
            Path::new("/home/x/.rust-dashcore-test/dashcore-24.0.0-rc.4/bin/dashd")
        );
        assert_eq!(binary("24.0.0-rc.4", Some(Path::new("/opt/d")), home), Path::new("/opt/d"));
    }

    /// A binary whose first `-version` line is not the version's is refused (`echo` standing in
    /// for dashd: its line is its arguments). The accepted line is the tier-1 run's.
    #[test]
    fn the_probe_wants_the_version_s_line() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let data = private_dir(dir.path(), "data").expect("a dir");
        // n-3: an existing 0700 directory serves; a wider one is refused.
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(private_dir(dir.path(), "data"), Ok(data.clone()));
            let wide = dir.path().join("wide");
            std::fs::create_dir(&wide).expect("a dir");
            std::fs::set_permissions(&wide, std::fs::Permissions::from_mode(0o775)).expect("chmod");
            assert!(private_dir(dir.path(), "wide").is_err_and(|e| e.contains("not a 0700")));
        }
        let e = probe(Path::new("/bin/echo"), &data, "24.0.0-rc.4").expect_err("not dashd");
        assert!(e.contains("-version\", not \"Dash Core version v24.0.0-rc.4\""), "{e}");
        let e = probe(&dir.path().join("none"), &data, "24.0.0-rc.4").expect_err("no binary");
        assert!(e.contains("none"), "{e}");
    }
}
