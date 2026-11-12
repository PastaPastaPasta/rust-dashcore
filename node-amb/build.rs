//! Embeds the commit the harness was built from, so a child of a stale test binary is told
//! apart from one built at the checkout's HEAD.

use std::process::Command;

#[path = "src/fnv.rs"]
mod fnv;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(env!("CARGO_MANIFEST_DIR"))
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn main() {
    if let Some(dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        println!("cargo:rerun-if-changed={dir}/HEAD");
    }
    if let (Some(common), Some(branch)) = (
        git(&["rev-parse", "--path-format=absolute", "--git-common-dir"]),
        git(&["symbolic-ref", "-q", "HEAD"]),
    ) {
        // A loose ref moves with each commit; `packed-refs` (shared by every worktree) only
        // matters when the branch has no loose ref.
        let loose = format!("{common}/{branch}");
        if std::path::Path::new(&loose).exists() {
            println!("cargo:rerun-if-changed={loose}");
        } else {
            println!("cargo:rerun-if-changed={common}/packed-refs");
        }
    }
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/fnv.rs");
    let rev = git(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=AMB_BUILD_REVISION={rev}");
    println!("cargo:rustc-env=AMB_BUILD_ENV={}", build_env());
}

/// What else shapes the code a build compiles, as one digest: the flags and environment Cargo
/// hands this build script (the same for every crate of the build but per-package profile
/// overrides, which `Cargo.toml` records) and the compiler's version.
fn build_env() -> String {
    const VARS: &[&str] = &[
        "CARGO_ENCODED_RUSTFLAGS",
        "RUSTFLAGS",
        "CARGO_BUILD_RUSTFLAGS",
        "OPT_LEVEL",
        "DEBUG",
        "PROFILE",
        "TARGET",
        "CARGO_CFG_TARGET_FEATURE",
        "CARGO_CFG_DEBUG_ASSERTIONS",
        "CARGO_CFG_OVERFLOW_CHECKS",
        "CARGO_CFG_PANIC",
        "RUSTC",
    ];
    let var = |v: &str| std::env::var(v).unwrap_or_default();
    let mut desc: Vec<String> = Vec::new();
    for v in VARS {
        println!("cargo:rerun-if-env-changed={v}");
        desc.push(format!("{v}={}", var(v)));
    }
    let mut profile: Vec<String> = std::env::vars()
        .filter(|(k, _)| k.starts_with("CARGO_PROFILE_"))
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    profile.sort();
    desc.extend(profile);
    let rustc = Some(var("RUSTC")).filter(|r| !r.is_empty()).unwrap_or_else(|| "rustc".into());
    let version = Command::new(rustc)
        .arg("-vV")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().replace('\n', " "))
        .unwrap_or_default();
    desc.push(version);
    format!("opt {} build {}", var("OPT_LEVEL"), fnv::fnv64(desc.join("\n").as_bytes()))
}
