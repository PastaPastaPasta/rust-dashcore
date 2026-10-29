//
// This file is a part of rust-dashcore.
// SPDX-License-Identifier: CC0-1.0
// See the accompanying file LICENSE or https://creativecommons.org/publicdomain/zero/1.0
//

//! Guards the "one base SDK revision" rule of the workspace `Cargo.toml`.
//!
//! base-sdk crates depend on each other, so two revisions in one graph build
//! two copies of `dash-num`/`dash-types` whose types do not unify.
//!
//! This reads only `Cargo.lock`. A Cargo config override (`paths`, source
//! replacement) changes the code that is built without changing the lockfile;
//! `contrib/node/check-base-sdk-pins.sh` checks the sources Cargo resolves.

use std::collections::BTreeMap;
use std::path::Path;

/// Packages published from `dashpay/base-sdk`, matched even when they come
/// from a path or a registry rather than the git repository.
const BASE_SDK_PACKAGES: &[&str] = &[
    "dash-dev",
    "dash-num",
    "dash-p2p-core",
    "dash-params",
    "dash-pkc",
    "dash-pow",
    "dash-primitives",
    "dash-script",
    "dash-types",
    "dash-types-marker",
];

const BASE_SDK_REPO: &str = "github.com/dashpay/base-sdk";

#[test]
fn base_sdk_packages_share_one_source() {
    // The lockfile records every package resolved for the workspace, across
    // all features and targets. `cargo test` writes it before running tests.
    let lockfile = Path::new(env!("CARGO_MANIFEST_DIR")).join("../Cargo.lock");
    let Ok(lockfile) = std::fs::read_to_string(&lockfile) else {
        eprintln!("skipping: no workspace lockfile at {}", lockfile.display());
        return;
    };

    let mut sources: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for package in lockfile.split("[[package]]").skip(1) {
        let field = |key: &str| {
            package.lines().find_map(|line| {
                line.strip_prefix(key)?.strip_prefix(" = ")?.strip_prefix('"')?.strip_suffix('"')
            })
        };
        let name = field("name").expect("package has a name");
        let source = field("source").unwrap_or("path");
        if BASE_SDK_PACKAGES.contains(&name) || source.contains(BASE_SDK_REPO) {
            sources.entry(source.to_owned()).or_default().push(name.to_owned());
        }
    }

    assert!(!sources.is_empty(), "no base SDK packages in the dependency graph");
    assert_eq!(sources.len(), 1, "base SDK packages resolve to several sources: {sources:#?}");
}
