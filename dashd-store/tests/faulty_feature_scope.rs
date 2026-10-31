//! ST3a design §3.1: the power-loss model is test-only. No workspace member, library or
//! binary, may build `dashd-store` with `faulty` or `raw-bytes` outside dev-dependencies.

use std::process::Command;

const FORBIDDEN: [&str; 2] =
    ["dashd-store feature \"faulty\"", "dashd-store feature \"raw-bytes\""];

/// `cargo tree` over the whole workspace with the given edge kinds.
fn tree(edges: &str) -> String {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = Command::new(cargo)
        .args(["tree", "--workspace", "-e", edges, "--target", "all", "--offline"])
        .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/.."))
        .output()
        .expect("cargo tree runs");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn faulty_and_raw_bytes_never_reach_a_normal_build() {
    let normal = tree("features,no-dev");
    for f in FORBIDDEN {
        assert!(!normal.contains(f), "a workspace member builds {f}");
    }
    // Control: the same query sees `raw-bytes` through the dev-dependency that enables it
    // (dash-coreimport's byte-identity tests), so it can fire. `faulty` has no dependency edge at
    // all: dashd-store's own suites require it (`required-features`), so a self dev-dependency
    // never turns it on in the benches.
    let with_dev = tree("features");
    assert!(with_dev.contains(FORBIDDEN[1]), "the check sees dev-only features");
    assert!(!with_dev.contains(FORBIDDEN[0]), "a dependency edge enables `faulty`");
}
