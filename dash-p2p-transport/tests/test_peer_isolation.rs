//! `test-peer` (`Bip324Cipher::encrypt_with_header`, a hostile peer's reserved header bits) stays
//! out of every root-workspace build. If a workspace crate enabled it, Cargo's feature unification
//! would turn it on for the gate's `cargo test --workspace` too, and the `compile_fail` doctest
//! guarding the default API would silently disappear. Only node-fuzz, a separate workspace, may
//! enable it; checking node-fuzz's manifest the same way shows the check finds an enabler.

use std::path::Path;
use std::process::Command;

use serde_json::Value;

const CRATE: &str = "dash-p2p-transport";
const FEATURE: &str = "test-peer";

/// The packages of the workspace at `manifest` (its members only) that enable `test-peer`: through
/// a dependency of any kind (renamed or not, optional or not) or one of their own features.
fn enablers(manifest: &Path) -> Vec<String> {
    let out = Command::new(env!("CARGO"))
        .args(["metadata", "--format-version", "1", "--no-deps", "--offline", "--manifest-path"])
        .arg(manifest)
        .output()
        .unwrap();
    assert!(out.status.success(), "cargo metadata: {}", String::from_utf8_lossy(&out.stderr));
    let metadata: Value = serde_json::from_slice(&out.stdout).unwrap();
    let mut found = Vec::new();
    for package in metadata["packages"].as_array().unwrap() {
        let name = package["name"].as_str().unwrap();
        let deps: Vec<&Value> = package["dependencies"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|d| d["name"] == CRATE)
            .collect();
        let by_dependency =
            deps.iter().any(|d| d["features"].as_array().unwrap().iter().any(|f| f == FEATURE));
        // `[features] x = ["dash-p2p-transport/test-peer"]`, under the dependency's rename if it
        // has one, and with `?` when it is optional.
        let keys: Vec<&str> = deps.iter().map(|d| d["rename"].as_str().unwrap_or(CRATE)).collect();
        let by_feature = package["features"]
            .as_object()
            .unwrap()
            .values()
            .flat_map(|v| v.as_array().unwrap())
            .filter_map(Value::as_str)
            .filter_map(|f| f.strip_suffix(&format!("/{FEATURE}")))
            .any(|dep| keys.contains(&dep.trim_end_matches('?')));
        if name != CRATE && (by_dependency || by_feature) {
            found.push(name.to_owned());
        }
    }
    found
}

#[test]
fn only_node_fuzz_enables_test_peer() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    assert_eq!(enablers(&root.join("Cargo.toml")), Vec::<String>::new(), "root workspace");
    assert_eq!(enablers(&root.join("node-fuzz/Cargo.toml")), ["node-fuzz"], "node-fuzz");
}
