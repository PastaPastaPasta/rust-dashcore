//! What dash-spv and dash-spv-ffi do with `log-file` and `env-filter` on and
//! off, as a host that depends on them sees it (SB3).
//!
//! `cfg!(feature = ...)` below is this crate's own feature, which forwards to
//! both libraries.

use std::ffi::{CStr, CString};
use std::path::Path;
use std::process::Command;

use dash_spv::logging::{init_logging, LogFileConfig, LoggingConfig};
use dash_spv::{LevelFilter, LoggingError};
use dash_spv_ffi::{
    dash_spv_ffi_client_destroy, dash_spv_ffi_client_new, dash_spv_ffi_config_destroy,
    dash_spv_ffi_config_set_data_dir, dash_spv_ffi_config_testnet, dash_spv_ffi_get_last_error,
    dash_spv_ffi_init_logging, FFIErrorCode, FFIEventCallbacks,
};

const INFO_MARKER: &str = "sb3-info-marker";
const DEBUG_MARKER: &str = "sb3-debug-marker";

fn last_error() -> String {
    let ptr = dash_spv_ffi_get_last_error();
    assert!(!ptr.is_null(), "no last error was set");
    unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned()
}

fn file_config(dir: &Path) -> LogFileConfig {
    LogFileConfig {
        log_dir: dir.to_path_buf(),
        max_files: 3,
    }
}

/// File logging through the Rust API: a log file with `log-file`, the
/// `FileLoggingUnavailable` error and nothing created without it.
#[test]
fn rust_api_file_logging() {
    let dir = tempfile::tempdir().unwrap();
    let log_dir = dir.path().join("logs");
    let result = init_logging(LoggingConfig {
        level: Some(LevelFilter::INFO),
        console: false,
        file: Some(file_config(&log_dir)),
        thread_local: true,
    });
    if cfg!(feature = "log-file") {
        let guard = result.expect("file logging with the log-file feature");
        tracing::info!("{INFO_MARKER}");
        drop(guard); // flushes the writer
        let content = std::fs::read_to_string(log_dir.join("run.log")).unwrap();
        assert!(content.contains(INFO_MARKER), "run.log: {content:?}");
    } else {
        assert!(
            matches!(result, Err(LoggingError::FileLoggingUnavailable)),
            "got {:?}",
            result.err()
        );
        assert!(!log_dir.exists(), "a refused request must not create the directory");
    }
}

/// The same through the C function, which installs the global subscriber: it
/// is the only test here that does, so one process-wide call is enough.
#[test]
fn ffi_file_logging() {
    let dir = tempfile::tempdir().unwrap();
    let log_dir = dir.path().join("ffi-logs");
    let level = CString::new("info").unwrap();
    let dir_c = CString::new(log_dir.to_str().unwrap()).unwrap();
    let code = unsafe { dash_spv_ffi_init_logging(level.as_ptr(), false, dir_c.as_ptr(), 3) };
    if cfg!(feature = "log-file") {
        assert_eq!(code, FFIErrorCode::Success as i32, "last error: {}", last_error());
        assert!(log_dir.join("run.log").exists());
    } else {
        assert_eq!(code, FFIErrorCode::RuntimeError as i32);
        assert_eq!(code, 8);
        let error = last_error();
        assert!(error.contains("log-file"), "last error: {error}");
        assert!(!log_dir.exists(), "a refused request must not create the directory");
    }
}

/// A null level means INFO, or RUST_LOG with `env-filter`. The subscriber is
/// process-wide, so the emitting side runs in a child process: this test, run
/// by `rust_log_is_honoured_only_with_env_filter` with RUST_LOG=debug.
#[test]
#[ignore = "run by rust_log_is_honoured_only_with_env_filter in a child process"]
fn child_emit_console_logs() {
    let _guard = init_logging(LoggingConfig {
        level: None,
        console: true,
        file: None,
        thread_local: false,
    })
    .expect("console logging");
    tracing::info!("{INFO_MARKER}");
    tracing::debug!("{DEBUG_MARKER}");
}

#[test]
fn rust_log_is_honoured_only_with_env_filter() {
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "child_emit_console_logs", "--nocapture"])
        .env("RUST_LOG", "debug")
        .output()
        .expect("child test process");
    assert!(output.status.success(), "child failed: {output:?}");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(text.contains(INFO_MARKER), "console logging printed nothing: {text}");
    assert_eq!(
        text.contains(DEBUG_MARKER),
        cfg!(feature = "env-filter"),
        "RUST_LOG=debug must be honoured exactly with env-filter: {text}"
    );
}

/// The trimmed tokio features still run an FFI client: the runtime builds, the
/// client opens its storage, and it stops on destroy.
#[test]
fn ffi_client_construct_and_destroy() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = CString::new(dir.path().to_str().unwrap()).unwrap();
    unsafe {
        let config = dash_spv_ffi_config_testnet();
        assert!(!config.is_null());
        assert_eq!(
            dash_spv_ffi_config_set_data_dir(config, data_dir.as_ptr()),
            FFIErrorCode::Success as i32
        );
        let client = dash_spv_ffi_client_new(config, FFIEventCallbacks::default());
        assert!(!client.is_null(), "client_new failed: {}", last_error());
        dash_spv_ffi_client_destroy(client);
        dash_spv_ffi_config_destroy(config);
    }
}
