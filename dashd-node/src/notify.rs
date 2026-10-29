//! The notify hooks: `-startupnotify`, `-shutdownnotify`, `-blocknotify` and `-alertnotify` run a
//! shell command through Core's `runCommand`. (`-chainlocknotify` is its owner's, wired through
//! [`run_command_detached`].)

use std::sync::Arc;
use std::thread::JoinHandle;

use dash_args::ArgsManager;
use dash_primitives::BlockHash;
use dashd_chainstate::chain::{SyncState, UiNotifications};

use crate::InitFailure;
use crate::logging::Logger;
use crate::warnings::Warnings;

/// Core's `runCommand`: `system(cmd)`, logging a nonzero status (the raw wait status, as Core
/// prints it).
// Core: util/system.cpp:1116-1126
pub fn run_command(logger: &Logger, cmd: &str) {
    if cmd.is_empty() {
        return;
    }
    let status = system(cmd);
    if status != 0 {
        logger.log_printf(&format!("runCommand error: system({cmd}) returned {status}\n"));
    }
}

#[cfg(unix)]
fn system(cmd: &str) -> i32 {
    // Core passes `c_str()`: the shell sees the text before any NUL.
    let cmd = cmd.split('\0').next().unwrap_or_default();
    let c = std::ffi::CString::new(cmd).expect("no NUL left");
    // SAFETY: `c` is a valid NUL-terminated string for the call.
    unsafe { libc::system(c.as_ptr()) }
}

/// `_wsystem`: `cmd.exe /c`, and its exit code.
#[cfg(not(unix))]
fn system(cmd: &str) -> i32 {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("cmd.exe")
        .arg("/c")
        .raw_arg(cmd)
        .status()
        .map_or(-1, |s| s.code().unwrap_or(-1))
}

/// `std::thread t(runCommand, cmd)`: a thread of its own (unnamed, as Core's).
fn spawn_command(logger: &Arc<Logger>, cmd: String) -> Result<JoinHandle<()>, InitFailure> {
    let logger = Arc::clone(logger);
    std::thread::Builder::new()
        .spawn(move || run_command(&logger, &cmd))
        .map_err(|e| InitFailure::Exception(e.to_string()))
}

/// `std::thread t(runCommand, cmd); t.detach()`: the command runs free.
pub fn run_command_detached(logger: &Arc<Logger>, cmd: String) -> Result<(), InitFailure> {
    spawn_command(logger, cmd).map(drop)
}

/// Core's `StartupNotify`, the last step of `AppInitMain`.
// Core: init.cpp:911-919
pub fn startup_notify(logger: &Arc<Logger>, args: &ArgsManager) -> Result<(), InitFailure> {
    let cmd = args.get_arg("-startupnotify", "")?;
    if cmd.is_empty() {
        return Ok(());
    }
    run_command_detached(logger, cmd)
}

/// V's `uiInterface` sink and the node's warnings.
struct NodeUi {
    logger: Arc<Logger>,
    /// `-blocknotify`: once init is over, each new best block runs the command with `%s`
    /// replaced by its hash, on a thread of its own. V calls it after each tip change it
    /// publishes: `ActivateBestChain`, `InvalidateBlock` and a ChainLock's reorg. `None` without
    /// it (or with it empty), as Core connects nothing then (init.cpp:2429-2438).
    block_notify: Option<String>,
    /// `-alertnotify`, which Core reads at each alert; nothing sets it after init.
    alert_notify: String,
    /// `g_misc_warnings`, which `GetWarnings` reports.
    warnings: Arc<Warnings>,
}

/// V's `uiInterface` sink: `-blocknotify`, the misc warning into `warnings` and `-alertnotify`.
pub fn chain_ui(
    logger: &Arc<Logger>,
    args: &ArgsManager,
    warnings: &Arc<Warnings>,
) -> Result<Arc<dyn UiNotifications>, InitFailure> {
    Ok(Arc::new(NodeUi::new(logger, args, warnings)?))
}

impl NodeUi {
    fn new(
        logger: &Arc<Logger>,
        args: &ArgsManager,
        warnings: &Arc<Warnings>,
    ) -> Result<Self, InitFailure> {
        let block_notify = args.get_arg("-blocknotify", "")?;
        Ok(NodeUi {
            logger: Arc::clone(logger),
            block_notify: (!block_notify.is_empty()).then_some(block_notify),
            alert_notify: args.get_arg("-alertnotify", "")?,
            warnings: Arc::clone(warnings),
        })
    }

    /// The command Core runs for a new tip: none during init, else every `%s` as the hash.
    // Core: init.cpp:2431-2436
    fn block_command(&self, state: SyncState, hash: BlockHash) -> Option<String> {
        let cmd = self.block_notify.as_ref()?;
        (state == SyncState::PostInit).then(|| cmd.replace("%s", &hash.to_string()))
    }

    /// The command Core's `AlertNotify` runs: none without `-alertnotify`, else every `%s` as
    /// the message, stripped to `SAFE_CHARS_DEFAULT` and single-quoted for the shell.
    // Core: validation.cpp:1998-2009
    fn alert_command(&self, message: &str) -> Option<String> {
        if self.alert_notify.is_empty() {
            return None;
        }
        let safe = format!("'{}'", sanitize_string(message));
        Some(self.alert_notify.replace("%s", &safe))
    }
}

impl UiNotifications for NodeUi {
    fn notify_block_tip(&self, state: SyncState, hash: BlockHash, _height: u32) {
        if let Some(cmd) = self.block_command(state, hash) {
            // A thread that cannot start runs nothing; Core's `std::thread` would throw into V.
            let _ = run_command_detached(&self.logger, cmd);
        }
    }

    // Core: warnings.cpp:24-28
    fn set_misc_warning(&self, warning: &str) {
        self.warnings.set_misc_warning(warning);
    }

    /// Core's `AlertNotify`. Its `uiInterface.NotifyAlertChanged()` reaches only the GUI's
    /// handlers, which dashd has none of.
    // Core: validation.cpp:1995-2013
    fn alert_notify(&self, message: &str) {
        if let Some(cmd) = self.alert_command(message) {
            let _ = run_command_detached(&self.logger, cmd);
        }
    }
}

/// Core's `SanitizeString(str)` with `SAFE_CHARS_DEFAULT`: the bytes of `s` that are ASCII letters,
/// digits or one of ` .,;-_/:?@()`, in order. A non-ASCII character's bytes are all dropped, so
/// filtering characters is filtering bytes.
// Core: util/strencodings.cpp:18-37
fn sanitize_string(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric() || " .,;-_/:?@()".contains(*c)).collect()
}

/// Core's `ShutdownNotify`, the first step of `Interrupt`: every `-shutdownnotify` command at
/// once, each on its own thread, all joined.
// Core: init.cpp:266-276
pub fn shutdown_notify(logger: &Arc<Logger>, args: &ArgsManager) {
    // `Interrupt` cannot fail; Core's `GetArgs` cannot throw here either (the arguments parsed).
    let cmds = args.get_args("-shutdownnotify").unwrap_or_default();
    let threads: Vec<_> = cmds.into_iter().filter_map(|c| spawn_command(logger, c).ok()).collect();
    for t in threads {
        let _ = t.join();
    }
}

#[cfg(all(test, unix))]
pub(crate) mod tests {
    use super::*;
    use crate::init::test_logger;

    #[test]
    fn a_failing_command_logs_its_wait_status() {
        let (logger, lines) = test_logger();
        run_command(&logger, "true");
        run_command(&logger, "");
        run_command(&logger, "exit 3");
        assert_eq!(*lines.lock().unwrap(), ["runCommand error: system(exit 3) returned 768\n"]);
    }

    #[test]
    fn shutdown_notify_runs_every_command_and_joins_them() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        let args = crate::init::test_args(&[
            &format!("-shutdownnotify=sleep 0.2; echo a > {}", a.display()),
            &format!("-shutdownnotify=echo b > {}", b.display()),
        ]);
        let (logger, _) = test_logger();
        shutdown_notify(&logger, &args);
        assert_eq!(std::fs::read_to_string(a).unwrap(), "a\n");
        assert_eq!(std::fs::read_to_string(b).unwrap(), "b\n");
    }

    /// Waits up to a minute for `path`, which a detached command writes.
    pub(crate) fn wait_for_file(path: &std::path::Path) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !path.exists() {
            assert!(std::time::Instant::now() < deadline, "{} never appeared", path.display());
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// The sink `argv`'s notify options make, over `warnings`.
    fn node_ui(argv: &[&str], warnings: &Arc<Warnings>) -> NodeUi {
        let (logger, _) = test_logger();
        NodeUi::new(&logger, &crate::init::test_args(argv), warnings).unwrap()
    }

    /// Only a tip after init has a command, with every `%s` as the hash; an empty
    /// `-blocknotify` connects nothing.
    #[test]
    fn block_notify_runs_after_init_with_the_hash() {
        let warnings = Arc::new(Warnings::new());
        let notify = node_ui(&["-blocknotify=echo %s-%s"], &warnings);
        let hash: BlockHash = format!("{:064x}", 3).parse().unwrap();
        assert_eq!(notify.block_command(SyncState::InitDownload, hash), None);
        assert_eq!(notify.block_command(SyncState::InitReindex, hash), None);
        let h = format!("{:064x}", 3);
        assert_eq!(notify.block_command(SyncState::PostInit, hash), Some(format!("echo {h}-{h}")));
        let unset = node_ui(&["-blocknotify="], &warnings);
        assert_eq!(unset.block_command(SyncState::PostInit, hash), None);
    }

    /// `-blocknotify` connects a sink that runs the command detached.
    #[test]
    fn chain_ui_runs_block_notify() {
        let dir = tempfile::tempdir().unwrap();
        let args =
            crate::init::test_args(&[&format!("-blocknotify=touch '{}'/%s", dir.path().display())]);
        let (logger, _) = test_logger();
        let hash: BlockHash = format!("{:064x}", 3).parse().unwrap();
        let ui = chain_ui(&logger, &args, &Arc::new(Warnings::new())).unwrap();
        ui.notify_block_tip(SyncState::PostInit, hash, 3);
        wait_for_file(&dir.path().join(format!("{:064x}", 3)));
    }

    /// `SAFE_CHARS_DEFAULT` (`strencodings.cpp:18`, `:22`): letters, digits and ` .,;-_/:?@()`.
    #[test]
    fn sanitize_string_keeps_cores_default_safe_characters() {
        let all: String = (0u8..=127).map(char::from).chain("é😀\u{a0}".chars()).collect();
        assert_eq!(
            sanitize_string(&all),
            " (),-./0123456789:;?@ABCDEFGHIJKLMNOPQRSTUVWXYZ_abcdefghijklmnopqrstuvwxyz"
        );
        assert_eq!(
            sanitize_string("Unknown new rules activated (versionbit 22)"),
            "Unknown new rules activated (versionbit 22)"
        );
    }

    /// `AlertNotify` replaces every `%s` with the sanitized message in single quotes, which no
    /// message can close; without `-alertnotify` it runs nothing.
    #[test]
    fn alert_notify_quotes_the_sanitized_message() {
        let warnings = Arc::new(Warnings::new());
        let notify = node_ui(&["-alertnotify=echo %s %s"], &warnings);
        assert_eq!(
            notify.alert_command("it's $(rm -rf ~); `x` \"q\"\n").as_deref(),
            Some("echo 'its (rm -rf ); x q' 'its (rm -rf ); x q'")
        );
        assert_eq!(node_ui(&[], &warnings).alert_command("x"), None);
        assert_eq!(node_ui(&["-alertnotify="], &warnings).alert_command("x"), None);
    }

    /// The sink stores the misc warning `GetWarnings` reports and runs `-alertnotify` detached.
    #[test]
    fn chain_ui_sets_the_misc_warning_and_runs_alert_notify() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("alert");
        let args =
            crate::init::test_args(&[&format!("-alertnotify=echo %s > '{}'", out.display())]);
        let (logger, _) = test_logger();
        let warnings = Arc::new(Warnings::new());
        let ui = chain_ui(&logger, &args, &warnings).unwrap();
        ui.set_misc_warning("Unknown new rules activated (versionbit 22)");
        assert_eq!(warnings.get_warnings(false), "Unknown new rules activated (versionbit 22)");
        ui.alert_notify("Unknown new rules activated (versionbit 22)");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while std::fs::read_to_string(&out).unwrap_or_default().is_empty() {
            assert!(std::time::Instant::now() < deadline, "the command never wrote");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            std::fs::read_to_string(&out).unwrap(),
            "Unknown new rules activated (versionbit 22)\n"
        );
    }
}
