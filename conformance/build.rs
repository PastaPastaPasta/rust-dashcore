//! Computes the code identity of this build, so checkpoints made by one build of the harness
//! or of the libraries under test are never reused by another (see `report::Identity`).
//!
//! The identity hashes:
//! - the source files of every workspace (path) package in the dependency closure of
//!   `dash-conformance` per `Cargo.lock` (the harness, `dashcore` and its path dependencies),
//!   excluding `target/`, `tests/`, `benches/`, `examples/`, `fuzz/`, dot-directories and
//!   Markdown files;
//! - the name, version and source of every other package in that closure (base-sdk is pinned
//!   by git revision, so its revision is part of the source);
//! - the target triple and build profile (`PROFILE`, `OPT_LEVEL`, `DEBUG`).
//!
//! Feature unification is not visible to a build script: a workspace build that enables extra
//! `dashcore` features gets the same identity.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// FNV-1a over 128 bits, as `report::Fnv128` (a build script cannot use the crate); enough to
/// notice a change, nothing adversarial.
struct Fnv(u128);

impl Fnv {
    fn new() -> Self {
        Fnv(0x6c62_272e_07bb_0142_62b8_2175_6295_c58d)
    }

    /// Adds one field, length-delimited so concatenations cannot collide.
    fn eat(&mut self, bytes: &[u8]) {
        const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;
        for b in bytes.iter().chain(&(bytes.len() as u64).to_le_bytes()) {
            self.0 = (self.0 ^ u128::from(*b)).wrapping_mul(PRIME);
        }
    }
}

struct Package {
    version: String,
    source: Option<String>,
    deps: Vec<String>,
}

/// Parses the `[[package]]` entries of a `Cargo.lock` (name -> versions).
fn parse_lock(lock: &str) -> BTreeMap<String, Vec<Package>> {
    let value = |l: &str| l.split_once('=').map(|(_, v)| v.trim().trim_matches('"').to_string());
    let mut out: BTreeMap<String, Vec<Package>> = BTreeMap::new();
    for entry in lock.split("[[package]]").skip(1) {
        let mut name = None;
        let mut pkg = Package {
            version: String::new(),
            source: None,
            deps: Vec::new(),
        };
        let mut in_deps = false;
        for line in entry.lines() {
            let line = line.trim();
            if in_deps {
                if line == "]" {
                    in_deps = false;
                } else {
                    pkg.deps.push(line.trim_end_matches(',').trim_matches('"').to_string());
                }
            } else if line.starts_with('[') {
                break; // another section ([[patch.unused]], [metadata])
            } else if line.starts_with("name =") {
                name = value(line);
            } else if line.starts_with("version =") {
                pkg.version = value(line).unwrap_or_default();
            } else if line.starts_with("source =") {
                pkg.source = value(line);
            } else if line.starts_with("dependencies = [") {
                in_deps = !line.ends_with(']');
            }
        }
        if let Some(name) = name {
            out.entry(name).or_default().push(pkg);
        }
    }
    out
}

/// Workspace member directories from the root `Cargo.toml` (`members = [...]`).
fn members(root: &Path) -> Vec<PathBuf> {
    let toml = std::fs::read_to_string(root.join("Cargo.toml")).expect("workspace Cargo.toml");
    let start = toml.find("members").expect("workspace members");
    let list = &toml[start..];
    let list =
        &list[list.find('[').expect("members list") + 1..list.find(']').expect("members end")];
    list.split(',')
        .map(|m| m.trim().trim_matches('"'))
        .filter(|m| !m.is_empty())
        .map(|m| root.join(m))
        .collect()
}

/// The `[package] name` of a crate directory.
fn package_name(dir: &Path) -> Option<String> {
    let toml = std::fs::read_to_string(dir.join("Cargo.toml")).ok()?;
    let pkg = &toml[toml.find("[package]")?..];
    let line = pkg.lines().find(|l| l.trim_start().starts_with("name"))?;
    Some(line.split_once('=')?.1.trim().trim_matches('"').to_string())
}

fn hash_dir(h: &mut Fnv, base: &Path, dir: &Path) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
        .map(|e| e.expect("dir entry").path())
        .collect();
    entries.sort();
    for path in entries {
        let name = path.file_name().expect("file name").to_string_lossy().to_string();
        if path.is_dir() {
            let skip = ["target", "tests", "benches", "examples", "fuzz"];
            if !name.starts_with('.') && !skip.contains(&name.as_str()) {
                hash_dir(h, base, &path);
            }
        } else if !name.ends_with(".md") {
            let rel = path.strip_prefix(base).expect("under base");
            h.eat(rel.to_string_lossy().as_bytes());
            h.eat(&std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())));
        }
    }
}

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let root = manifest.parent().expect("workspace root").to_path_buf();
    let lock_path = root.join("Cargo.lock");
    println!("cargo:rerun-if-changed={}", lock_path.display());
    println!("cargo:rerun-if-changed=build.rs");
    let lock = parse_lock(&std::fs::read_to_string(&lock_path).expect("Cargo.lock"));

    // Dependency closure of this crate.
    let mut closure: BTreeSet<(String, usize)> = BTreeSet::new();
    let mut stack = vec![("dash-conformance".to_string(), 0)];
    while let Some(id) = stack.pop() {
        if !closure.insert(id.clone()) {
            continue;
        }
        for dep in &lock[&id.0][id.1].deps {
            // `name`, `name version` or `name version (source)`, as specific as needed to
            // tell apart several packages of one name (e.g. two base-sdk revisions).
            let mut parts = dep.splitn(3, ' ');
            let name = parts.next().expect("dep name").to_string();
            let version = parts.next();
            let source = parts.next().map(|s| s.trim_start_matches('(').trim_end_matches(')'));
            let candidates = &lock[&name];
            let idx = candidates
                .iter()
                .position(|p| {
                    version.is_none_or(|v| p.version == v)
                        && source.is_none_or(|s| {
                            p.source.as_deref().is_some_and(|ps| ps.split('#').next() == Some(s))
                        })
                })
                .unwrap_or_else(|| panic!("Cargo.lock: no package matches dependency {dep}"));
            stack.push((name, idx));
        }
    }

    let dirs: BTreeMap<String, PathBuf> =
        members(&root).into_iter().filter_map(|d| Some((package_name(&d)?, d))).collect();
    let mut h = Fnv::new();
    let mut base_sdk = BTreeSet::new();
    for (name, idx) in &closure {
        let pkg = &lock[name][*idx];
        h.eat(name.as_bytes());
        h.eat(pkg.version.as_bytes());
        match &pkg.source {
            Some(source) => {
                h.eat(source.as_bytes());
                if source.contains("base-sdk") {
                    let rev = source.rsplit('#').next().unwrap_or(source);
                    base_sdk.insert(rev.to_string());
                }
            }
            None => {
                let dir = dirs.get(name).unwrap_or_else(|| panic!("no workspace member {name}"));
                println!("cargo:rerun-if-changed={}", dir.display());
                hash_dir(&mut h, &root, dir);
            }
        }
    }
    for var in ["TARGET", "PROFILE", "OPT_LEVEL", "DEBUG"] {
        h.eat(std::env::var(var).unwrap_or_default().as_bytes());
    }
    let base_sdk: Vec<_> = base_sdk.into_iter().collect();
    println!("cargo:rustc-env=CONFORMANCE_CODE_ID={:032x}", h.0);
    println!("cargo:rustc-env=CONFORMANCE_BASE_SDK={}", base_sdk.join(","));
}
