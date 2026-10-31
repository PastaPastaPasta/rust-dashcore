//! Links libdash_oracle for every pin in pins.txt when the `oracle` feature is on.
//!
//! The archives come from `contrib/node/build-oracle.sh` (Linux, GNU binutils),
//! cached under `$DASH_ORACLE_CACHE/<commit>/<subdir>/` (default `/work/cache/oracle`; `<subdir>` is
//! the flavor, or `fuzz-nocmp` for fuzz: see `archive::cache_dir_name`).
//! Each one is copied into `OUT_DIR`, the copy is hashed, and only a copy whose
//! sha256 is the one pins.txt records is linked; anything else fails the build.
//!
//! - `DASH_ORACLE_AUTOBUILD=1` runs the script for missing archives or archives that
//!   fail the check (several minutes per pin, once). Without it a missing archive
//!   only skips linking, with a warning, so `cargo check/clippy/doc --all-features`
//!   work anywhere; linking then fails.
//! - `DASH_ORACLE_FLAVOR`: `plain` (default), `fuzz` (coverage-instrumented for libFuzzer) or
//!   `ubsan` (UndefinedBehaviorSanitizer; run with the `UBSAN_OPTIONS` the README gives,
//!   so the first report aborts; development only, never pinned, so it needs
//!   `DASH_ORACLE_ALLOW_UNPINNED=1`).
//! - `DASH_ORACLE_ALLOW_UNPINNED=1` links archives whose sha256 pins.txt does not
//!   record (development builds, `build-oracle.sh --dev`). The crate then reports
//!   `ARCHIVES_PINNED == false`, which the smoke tests and consensus gates reject.
//! - `DASH_SRC`: the Dash Core repository holding the pinned commits (default `~/workspace/dash`).

#[path = "build/archive.rs"]
mod archive;

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=build/archive.rs");
    println!("cargo:rerun-if-changed=pins.txt");
    println!("cargo::rustc-check-cfg=cfg(dash_oracle_unpinned)");
    for var in [
        "DASH_ORACLE_CACHE",
        "DASH_ORACLE_FLAVOR",
        "DASH_ORACLE_AUTOBUILD",
        "DASH_ORACLE_ALLOW_UNPINNED",
        "DASH_SRC",
    ] {
        println!("cargo:rerun-if-env-changed={var}");
    }
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let pins_txt = manifest_dir.join("pins.txt");
    let text = std::fs::read_to_string(&pins_txt)
        .unwrap_or_else(|e| panic!("read {}: {e}", pins_txt.display()));
    let pins = archive::parse_pins(&text).unwrap_or_else(|e| panic!("{e}"));
    if env::var_os("CARGO_FEATURE_ORACLE").is_none() {
        return;
    }

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let cache = env::var("DASH_ORACLE_CACHE").unwrap_or_else(|_| "/work/cache/oracle".into());
    let flavor = env::var("DASH_ORACLE_FLAVOR").unwrap_or_else(|_| "plain".into());
    let autobuild = env::var("DASH_ORACLE_AUTOBUILD").is_ok_and(|v| v == "1");
    let allow_unpinned = env::var("DASH_ORACLE_ALLOW_UNPINNED").is_ok_and(|v| v == "1");
    let script = manifest_dir.join("../contrib/node/build-oracle.sh");
    let build = |pin: &archive::Pin| {
        println!(
            "cargo:warning=building the Dash Core oracle for {} ({}); this takes several minutes once",
            pin.name, pin.commit
        );
        let status = Command::new("bash")
            .arg(&script)
            .args(["--flavor", &flavor, "--cache", &cache, &pin.name])
            .status()
            .expect("run build-oracle.sh");
        assert!(
            status.success(),
            "build-oracle.sh failed for {}; see {cache}/logs/{}-{}.log",
            pin.name,
            pin.commit,
            archive::cache_dir_name(&flavor)
        );
    };

    let mut snapshots = Vec::new();
    for pin in &pins {
        let dir = Path::new(&cache).join(&pin.commit).join(archive::cache_dir_name(&flavor));
        let cached = dir.join(pin.archive_name());
        if !cached.exists() && autobuild {
            build(pin);
        }
        if !cached.exists() {
            println!(
                "cargo:warning={} is missing, so dash-oracle-sys links nothing; run {} {} or set DASH_ORACLE_AUTOBUILD=1",
                cached.display(),
                script.display(),
                pin.name
            );
            println!("cargo:rustc-cfg=dash_oracle_unpinned"); // nothing was verified
            return;
        }
        println!("cargo:rerun-if-changed={}", cached.display());
        println!("cargo:rerun-if-changed={}", dir.join("MANIFEST").display());
        let dest = out_dir.join(pin.archive_name());
        let snapshot = archive::snapshot(&dir, pin, &flavor, &dest, allow_unpinned)
            .or_else(|e| {
                if !autobuild {
                    return Err(e);
                }
                println!("cargo:warning={e}");
                build(pin);
                archive::snapshot(&dir, pin, &flavor, &dest, allow_unpinned)
            })
            .unwrap_or_else(|e| panic!("dash-oracle-sys: {e}"));
        if !snapshot.pinned {
            println!(
                "cargo:warning=linking UNPINNED {} oracle archive (sha256 {}); results do not stand for the pinned Core release",
                pin.name, snapshot.sha256
            );
        }
        snapshots.push((pin, snapshot));
    }

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    for (pin, snapshot) in &snapshots {
        println!("cargo:rustc-link-lib=static={}", pin.lib_name());
        println!(
            "cargo:rustc-env=DASH_ORACLE_SHA256_{}={}",
            pin.name.replace('.', "_").to_uppercase(),
            snapshot.sha256
        );
    }
    if snapshots.iter().any(|(_, s)| !s.pinned) {
        println!("cargo:rustc-cfg=dash_oracle_unpinned");
    }
    println!("cargo:rustc-link-lib=dylib=stdc++");
    if flavor == "ubsan" {
        // The archive's sanitizer checks call into gcc's UBSan runtime.
        println!("cargo:rustc-link-lib=dylib=ubsan");
    }
}
