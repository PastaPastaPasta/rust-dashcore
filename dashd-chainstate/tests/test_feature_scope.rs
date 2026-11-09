//! CH2 §6.7 (ST3a §3.1's `cargo tree` guard, extended): the crate's test features never reach a
//! normal build. No workspace member, library or binary, may enable `alloc-tag`, `crash`, `model`
//! or `planted-bugs` outside dev-dependencies (node-fuzz, outside the workspace, may). ST3a's own
//! guard covers `dashd-store`'s `faulty`, which `crash` enables.

use std::process::Command;

const FORBIDDEN: [&str; 4] = [
    "dashd-chainstate feature \"alloc-tag\"",
    "dashd-chainstate feature \"crash\"",
    "dashd-chainstate feature \"model\"",
    "dashd-chainstate feature \"planted-bugs\"",
];

/// `cargo tree`, inverted on this crate, so it lists the crate's enabled features and who enables
/// them.
fn tree(args: &[&str]) -> String {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = Command::new(cargo)
        .arg("tree")
        .args(args)
        .args(["-e", "features,no-dev", "--target", "all", "--offline", "-i", "dashd-chainstate"])
        .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/.."))
        .output()
        .expect("cargo tree runs");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn test_features_never_reach_a_normal_build() {
    let normal = tree(&["--workspace"]);
    for f in FORBIDDEN {
        assert!(!normal.contains(f), "a workspace member builds {f}:\n{normal}");
    }
    // Control: the same query names them when they are on (the gate's test features), so it can
    // fire.
    let on = tree(&["-p", "dashd-chainstate", "--features", "alloc-tag,crash,planted-bugs"]);
    assert!(FORBIDDEN.iter().all(|f| on.contains(f)), "the check sees the features:\n{on}");
}

/// ST3b's plant build (plan §3.7): rdashd passes `planted-bugs` through dashd-node, and the
/// product rdashd, built without the feature, carries none of it.
#[test]
fn rdashd_carries_planted_bugs_only_in_the_plant_build() {
    let product = tree(&["-p", "rdashd"]);
    assert!(
        !product.contains(FORBIDDEN[3]),
        "the product rdashd builds {}:\n{product}",
        FORBIDDEN[3]
    );
    let plant = tree(&["-p", "rdashd", "--features", "planted-bugs"]);
    assert!(plant.contains(FORBIDDEN[3]), "the plant build passes the feature through:\n{plant}");
}
