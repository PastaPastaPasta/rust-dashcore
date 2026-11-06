//! Fault injection for the process tests (`tests/hooks.rs`), built only with the `test-hooks`
//! feature. `RDASHD_TEST_HOOK` picks one:
//! - `term-during-init`: SIGTERM through the real handler just before `AppInitMain`;
//! - `abort-after-init`: an `AbortNode` once init is done;
//! - `wal-sync-fails-after-init`: the chainstate's next WAL sync fails (shutdown's first flush);
//! - `read-failed-after-init`: CH2's coins-read failure once init is done;
//! - `rng-order`: one line saying whether the process RNG's state exists just before the daemon
//!   step (`tests/hooks.rs`, BL-502 test 7);
//! - `hold-warmup`: the RPC server stays in warmup until `release-warmup` appears in the network
//!   data directory (`dashd_node`'s hook, as warmup ends inside init);
//! - `f01-park`: V makes the block `RDASHD_TEST_HOOK_BLOCK` names durable at the start of its ABC
//!   tail, writes `f01-ready` in the network data directory and parks (`dashd_node`'s hook, armed
//!   before the warmup ends; `tests/f01_restart.rs`, CH3b D223);
//! - `v-panic-at-block`: V panics at the start of the ABC tail of the block
//!   `RDASHD_TEST_HOOK_BLOCK` names (`dashd_node`'s hook, armed before the warmup ends;
//!   `tests/hooks.rs`, BL-586);
//! - `v-panic-in-first-shutdown-flush`, `v-panic-in-second-shutdown-flush`: V's flush side panics
//!   before its job in that shutdown flush (BL-674).

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

/// Just before the daemon step: whether the process RNG's state exists yet.
pub fn before_daemonize(node: &Node) {
    if hook().as_deref() == Some("rng-order") {
        node.ui.logger().log_printf(&format!(
            "test-hooks: process RNG state before the daemon step: {}\n",
            dashd_random::state_created()
        ));
    }
}

/// After a successful init, before `main` waits for the shutdown.
pub fn after_init(node: &mut Node) {
    match hook().as_deref() {
        Some("abort-after-init") => node.abort.abort_node("test-hooks: injected abort", None),
        Some("read-failed-after-init") => node.abort.read_failed("test-hooks: injected read"),
        Some("wal-sync-fails-after-init") => node.fail_next_wal_sync(),
        Some("v-panic-in-first-shutdown-flush") => node.panic_v_in_shutdown_flush(false),
        Some("v-panic-in-second-shutdown-flush") => node.panic_v_in_shutdown_flush(true),
        _ => {}
    }
}
