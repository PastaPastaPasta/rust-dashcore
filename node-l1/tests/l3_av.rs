//! The AV fixture pairs (plan §7.4, M-10) against a real regtest dashd v24 and V, each replayed
//! on the shim ([`node_l1::l3::run_av`]). A census component outside tier 1; it keeps its report
//! (`L3_AV_REPORT`) and fails closed.

use std::path::{Path, PathBuf};

fn repo() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/.."))
}

#[test]
fn the_av_fixtures_skip_only_what_assumevalid_skips() {
    let scratch = tempfile::tempdir().expect("a temp dir");
    let report = std::env::var_os("L3_AV_REPORT").map(PathBuf::from);
    let (value, done) =
        node_l1::l3::run_av(repo(), scratch.path(), report.as_deref()).expect("the AV run started");
    if !done {
        let errors: Vec<_> = value["legs"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|l| l["status"] != "done")
            .map(|l| format!("{}: {}", l["name"], l["error"]))
            .collect();
        let kept = scratch.keep();
        panic!("the AV fixtures failed (kept {}): {errors:?}", kept.display());
    }
}
