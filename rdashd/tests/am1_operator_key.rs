//! A production rdashd refuses `-masternodeblsprivkey` until DK1, and logs the key nowhere.
// Core: init.cpp:2156-2167 at v24.0.0
#![cfg(unix)]

mod common;

use common::*;

/// A regtest key ends init with rdashd's refusal; the key's hex reaches neither `debug.log`,
/// the console sink (stdout, `-printtoconsole=1`) nor stderr, and the command-line log line
/// masks it.
#[test]
fn a_regtest_key_is_refused_and_its_hex_is_never_logged() {
    let (home, datadir) = home_and_datadir();
    let key = "01".repeat(32);
    let arg = format!("-masternodeblsprivkey={key}");
    let out = run_regtest(
        home.path(),
        &datadir,
        &[&arg, "-blocksonly=0", "-dnsseed=0", "-fixedseeds=0", "-printtoconsole=1"],
    );
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let refusal = "-masternodeblsprivkey is not supported by rdashd yet (DK1).";
    assert_eq!(stderr(&out), format!("Error: {refusal}\n"));
    let log = read(&debug_log(&datadir));
    let console = stdout(&out);
    for text in [&log, &console] {
        assert!(text.contains(refusal), "{text}");
        assert!(text.contains("masternodeblsprivkey=****"), "{text}");
        assert!(!text.contains("MASTERNODE:"), "{text}");
    }
    for text in [&log, &console, &stderr(&out)] {
        assert!(!text.to_ascii_lowercase().contains(&key), "the key's hex was logged");
    }
}
