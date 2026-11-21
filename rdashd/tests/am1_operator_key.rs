//! A production rdashd runs as an active masternode with `-masternodeblsprivkey`, and logs the
//! key nowhere.
// Core: init.cpp:2156-2167, :2538-2547; active/masternode.cpp:60-62 at v24.0.0
#![cfg(unix)]

mod common;

use common::*;

/// A regtest key makes the binary an active masternode: init completes, the log carries the
/// operator public key, and the key's hex reaches neither `debug.log`, the console sink (stdout,
/// `-printtoconsole=1`) nor stderr, while the command-line log line masks it.
#[test]
fn a_regtest_key_runs_an_active_masternode_and_its_hex_is_never_logged() {
    let (home, datadir) = home_and_datadir();
    let key = "01".repeat(32);
    let arg = format!("-masternodeblsprivkey={key}");
    let node = start(
        home.path(),
        &datadir,
        &[&arg, "-blocksonly=0", "-dnsseed=0", "-fixedseeds=0", "-printtoconsole=1"],
    );
    let out = node.stop_ok(libc::SIGTERM);
    let log = read(&debug_log(&datadir));
    let console = stdout(&out);
    for text in [&log, &console] {
        assert!(!text.contains("is not supported by rdashd yet"), "{text}");
        assert!(text.contains("masternodeblsprivkey=****"), "{text}");
        assert!(text.contains("MASTERNODE:"), "{text}");
    }
    for text in [&log, &console, &stderr(&out)] {
        assert!(!text.to_ascii_lowercase().contains(&key), "the key's hex was logged");
    }
}
