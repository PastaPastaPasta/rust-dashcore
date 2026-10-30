//! The no-GUI user interface (`noui.cpp`): what `InitError`, `InitWarning` and `InitMessage`
//! print.

use std::sync::{Arc, Mutex, PoisonError};

use crate::logging::Logger;

type Sink = Box<dyn Fn(&str) + Send + Sync>;

/// Core's `noui_connect` handlers.
pub struct Ui {
    logger: Arc<Logger>,
    stderr: Sink,
    /// `InitMessage_connect` subscribers (the RPC warmup status).
    init_message_hooks: Mutex<Vec<Sink>>,
}

impl Ui {
    /// Writes to the process's stderr.
    pub fn new(logger: Arc<Logger>) -> Ui {
        Ui::with_stderr(logger, Box::new(|s| eprint!("{s}")))
    }

    /// Writes what Core sends to `std::cerr` to `stderr` instead.
    pub fn with_stderr(logger: Arc<Logger>, stderr: Sink) -> Ui {
        Ui {
            logger,
            stderr,
            init_message_hooks: Mutex::new(Vec::new()),
        }
    }

    pub fn logger(&self) -> &Arc<Logger> {
        &self.logger
    }

    /// Core's `InitError`: `Error: <msg>` to the log and to stderr.
    // Core: noui.cpp:22-49 (MSG_ERROR, not SECURE)
    pub fn init_error(&self, msg: &str) {
        let line = dash_args::init::noui_error_line(msg);
        self.logger.log_printf(&line);
        (self.stderr)(&line);
    }

    /// Core's `InitWarning`: `Warning: <msg>` to the log and to stderr.
    pub fn init_warning(&self, msg: &str) {
        let line = dash_args::init::noui_warning_line(msg);
        self.logger.log_printf(&line);
        (self.stderr)(&line);
    }

    /// Core's `ThreadSafeQuestion(..., MSG_ERROR | BTN_ABORT)` without a GUI: the style matches
    /// no caption, so the line starts with an empty caption and `: `. The answer is no.
    // Core: noui.cpp:22-54
    pub fn thread_safe_question(&self, non_interactive_msg: &str) {
        let line = format!(": {non_interactive_msg}\n");
        self.logger.log_printf(&line);
        (self.stderr)(&line);
    }

    /// Core's `InitMessage`: `noui_InitMessage` writes it to the log only, then every
    /// [`connect_init_message`](Self::connect_init_message) subscriber gets it.
    // Core: noui.cpp:56-59
    pub fn init_message(&self, msg: &str) {
        self.logger.log_printf(&format!("init message: {msg}\n"));
        for hook in self.init_message_hooks.lock().unwrap_or_else(PoisonError::into_inner).iter() {
            hook(msg);
        }
    }

    /// Core's `uiInterface.InitMessage_connect`.
    pub fn connect_init_message(&self, f: impl Fn(&str) + Send + Sync + 'static) {
        self.init_message_hooks.lock().unwrap_or_else(PoisonError::into_inner).push(Box::new(f));
    }
}
