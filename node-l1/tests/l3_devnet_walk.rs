//! The devnet leg's X/Y/Z period walk (plan §7.2, M-8 / OD-L3-14) against real devnet dashd v24
//! processes, with V's warning checker run in process over Core's headers ([`devnet_walk`]). A
//! nightly component outside tier 1 (≈ 16,000 blocks); it keeps its report (`L3_WALK_REPORT`)
//! and fails closed.

use std::path::Path;

use node_l1::l3::devnet_walk;
use node_l1::l3::version;

fn repo() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/.."))
}

#[test]
fn the_devnet_walk_skips_the_period_below_its_minimum() {
    let scratch = tempfile::tempdir().expect("a temp dir");
    let dashd = version::resolve(repo(), scratch.path()).expect("the run's dashd (plan §3.1)");
    let log = devnet_walk::run(&dashd, scratch.path());
    let report = log.report();
    if let Ok(path) = std::env::var("L3_WALK_REPORT") {
        let text = serde_json::to_string_pretty(&report).expect("a report");
        std::fs::write(&path, text).unwrap_or_else(|e| panic!("{path}: {e}"));
    }
    if let Some(e) = log.failure() {
        let kept = scratch.keep();
        panic!("the devnet walk failed (kept {}): {e}", kept.display());
    }
}
