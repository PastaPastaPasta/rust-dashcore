//! Fault injection for the process tests (`tests/hooks.rs`), built only with the `test-hooks`
//! feature. `RDASHD_TEST_HOOK` picks one:
//! - `term-during-init`: SIGTERM through the real handler just before `AppInitMain`;
//! - `abort-after-init`: an `AbortNode` once init is done;
//! - `wal-sync-fails-after-init`: the chainstate's next WAL sync fails (shutdown's first flush);
//! - `read-failed-after-init`: CH2's coins-read failure once init is done;
//! - `hold-warmup`: the RPC server stays in warmup until `release-warmup` appears in the network
//!   data directory (`dashd_node`'s hook, as warmup ends inside init).

use std::time::{Duration, Instant};

use dashd_node::Node;
use dashd_node::abort::AbortSink as _;

fn hook() -> Option<String> {
    std::env::var("RDASHD_TEST_HOOK").ok()
}

/// Before `AppInitMain`: the signal arrives during init, and init goes on only once the signals
/// thread has acted on it.
pub fn before_app_init_main(node: &Node) {
    if hook().as_deref() != Some("term-during-init") {
        return;
    }
    // SAFETY: raising a signal whose handler is installed.
    #[cfg(unix)]
    unsafe {
        libc::raise(libc::SIGTERM);
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    while !node.shutdown.shutdown_requested() {
        assert!(Instant::now() < deadline, "the SIGTERM was not acted on");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// After a successful init, before `main` waits for the shutdown.
pub fn after_init(node: &Node) {
    match hook().as_deref() {
        Some("abort-after-init") => node.abort.abort_node("test-hooks: injected abort", None),
        Some("read-failed-after-init") => node.abort.read_failed("test-hooks: injected read"),
        Some("wal-sync-fails-after-init") => node.fail_next_wal_sync(),
        _ => {}
    }
}
