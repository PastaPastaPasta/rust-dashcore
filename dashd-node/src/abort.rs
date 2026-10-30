//! `AbortNode` (`shutdown.cpp`) for the node's crates: [`NodeAbort`] is the one sink every
//! crate's abort seam takes, CH2's store and CH1-node's V alike (DEC-201 Q2), through the one
//! `dash_kv` trait (BL-109, SB10 Q4).

use std::sync::{Arc, Mutex, PoisonError};

pub use dashd_chainstate::store::AbortSink;
use dashd_chainstate::store::CoreAbortSink;

use crate::shutdown::ShutdownState;
use crate::ui::Ui;
use crate::warnings::Warnings;

/// Core's default `AbortNode` user message.
pub const DEFAULT_ABORT_USER_MESSAGE: &str =
    "A fatal internal error occurred, see debug.log for details";

/// Core's `AbortNode`, plus CH2's coins-read failure.
pub struct NodeAbort {
    core: CoreAbortSink,
    /// Every `AbortNode`'s log message, in report order.
    reported: Mutex<Vec<String>>,
}

impl NodeAbort {
    pub fn new(ui: Arc<Ui>, warnings: Arc<Warnings>, shutdown: Arc<ShutdownState>) -> NodeAbort {
        let logger = Arc::clone(ui.logger());
        let core = CoreAbortSink::new(
            move |line| logger.log_printf(&format!("{line}\n")),
            move |log_msg, user_msg| abort_node(&ui, &warnings, &shutdown, log_msg, user_msg),
        );
        NodeAbort {
            core,
            reported: Mutex::new(Vec::new()),
        }
    }

    /// How many `AbortNode`s have been reported.
    pub fn reports(&self) -> usize {
        self.reported.lock().unwrap_or_else(PoisonError::into_inner).len()
    }

    /// The log message of report `n` (from 0), if made: Core's `state.ToString()` after
    /// `AbortNode(state, msg)`, which `ForceFlushStateToDisk`'s failure line shows.
    // Core: validation.cpp:2245-2249, :3132
    pub fn report(&self, n: usize) -> Option<String> {
        self.reported.lock().unwrap_or_else(PoisonError::into_inner).get(n).cloned()
    }
}

/// Core's `AbortNode`: the misc warning, `*** <msg>` in the log, the user message as an
/// `InitError`, exit status 1, and a shutdown request. The shutdown still flushes (DEC-241).
// Core: shutdown.cpp:26-37
fn abort_node(
    ui: &Ui,
    warnings: &Warnings,
    shutdown: &ShutdownState,
    log_msg: &str,
    user_msg: Option<&str>,
) {
    warnings.set_misc_warning(log_msg);
    ui.logger().log_printf(&format!("*** {log_msg}\n"));
    ui.init_error(user_msg.filter(|m| !m.is_empty()).unwrap_or(DEFAULT_ABORT_USER_MESSAGE));
    shutdown.mark_aborted();
    shutdown.start_shutdown();
}

impl AbortSink for NodeAbort {
    fn abort_node(&self, log_msg: &str, user_msg: Option<&str>) {
        self.reported.lock().unwrap_or_else(PoisonError::into_inner).push(log_msg.to_owned());
        self.core.abort_node(log_msg, user_msg);
    }

    /// CH2's: two lines and `abort()`, with no shutdown.
    fn read_failed(&self, what: &str) {
        self.core.read_failed(what);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::logging::Logger;

    #[test]
    fn abort_node_warns_logs_errors_and_requests_a_shutdown() {
        let logger = Arc::new(Logger::new());
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&lines);
        logger.push_back_callback(move |s| sink.lock().unwrap().push(s.to_owned()));
        assert!(logger.start_logging());
        let stderr = Arc::new(Mutex::new(String::new()));
        let err = Arc::clone(&stderr);
        let ui = Arc::new(Ui::with_stderr(
            Arc::clone(&logger),
            Box::new(move |s| err.lock().unwrap().push_str(s)),
        ));
        let warnings = Arc::new(Warnings::new());
        let shutdown = Arc::new(ShutdownState::new());
        let abort = NodeAbort::new(ui, Arc::clone(&warnings), Arc::clone(&shutdown));

        AbortSink::abort_node(&abort, "Failed to write block", None);
        assert_eq!(warnings.get_warnings(false), "Failed to write block");
        assert!(shutdown.shutdown_requested() && shutdown.aborted());
        assert_eq!(shutdown.exit_status(), 1);
        AbortSink::abort_node(&abort, "Disk full", Some("Disk space is too low!"));
        // V's sink is the same `AbortNode`: it passes no user message, so Core's default shows.
        let v: &dyn dashd_chainstate::chain::AbortSink = &abort;
        v.abort_node("Failed to read block", None);
        let lines = lines.lock().unwrap();
        let lines: Vec<_> = lines.iter().map(|l| l.split_once("Z ").unwrap().1).collect();
        assert_eq!(
            lines,
            [
                "*** Failed to write block\n",
                "Error: A fatal internal error occurred, see debug.log for details\n",
                "*** Disk full\n",
                "Error: Disk space is too low!\n",
                "*** Failed to read block\n",
                "Error: A fatal internal error occurred, see debug.log for details\n",
            ]
        );
        assert_eq!(
            *stderr.lock().unwrap(),
            "Error: A fatal internal error occurred, see debug.log for details\n\
             Error: Disk space is too low!\n\
             Error: A fatal internal error occurred, see debug.log for details\n"
        );
    }
}
