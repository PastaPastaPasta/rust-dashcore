//! The SW-UPSTREAM firewall (LQ4 §6): only `dkg::wire` names BS9a's message types and
//! `dash_primitives`, and only `dkg::wire` and `dkg::bls` name the base SDK's P2P and BLS
//! crates. A rebase onto a new base SDK then touches those two modules alone.

use std::fs;
use std::path::{Path, PathBuf};

/// Paths, relative to `src/`, that may name the base SDK: the modules and their test files.
const ALLOWED: [&str; 4] = ["dkg/wire.rs", "dkg/wire/", "dkg/bls.rs", "dkg/bls/"];

/// Base SDK crates and BS9a type names.
const BANNED: [&str; 11] = [
    "dash_p2p_core",
    "dash_primitives",
    "dash_pkc",
    "DkgContribution",
    "DkgComplaint",
    "DkgJustification",
    "DkgPrematureCommitment",
    "DkgRevealedShare",
    "DkgMessage",
    "BlsPkBytes",
    "BlsSigBytes",
];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("readable source tree") {
        let path = entry.expect("a directory entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn only_wire_and_bls_name_the_base_sdk() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    assert!(files.len() > 3, "the scan sees the crate");
    let mut hits = Vec::new();
    for file in files {
        let rel = file.strip_prefix(&src).expect("under src").to_string_lossy().replace('\\', "/");
        if ALLOWED.iter().any(|allowed| rel.starts_with(allowed)) {
            continue;
        }
        let text = fs::read_to_string(&file).expect("readable source");
        for (n, line) in text.lines().enumerate() {
            for banned in BANNED {
                if line.contains(banned) {
                    hits.push(format!("src/{rel}:{}: {banned}", n + 1));
                }
            }
        }
    }
    assert!(hits.is_empty(), "base SDK names outside dkg::wire and dkg::bls:\n{}", hits.join("\n"));
}
