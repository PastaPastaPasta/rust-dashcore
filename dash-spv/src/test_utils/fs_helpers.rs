//! Filesystem helpers for test infrastructure.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Recursively copy a directory and all its contents.
pub(super) fn copy_dir(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let dst_path = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &dst_path)?;
        } else {
            fs::copy(entry.path(), dst_path)?;
        }
    }
    Ok(())
}

/// When `DASHD_TEST_RETAIN_DIR` is set, copy `src` to a test-named
/// subdirectory for post-mortem inspection.
///
/// By default only retains on panic. Set `DASHD_TEST_RETAIN_ALWAYS=1`
/// to also retain directories from passing tests.
pub fn retain_test_dir(src: &Path, label: &str) {
    let retain_always = std::env::var("DASHD_TEST_RETAIN_ALWAYS")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    if !retain_always && !std::thread::panicking() {
        return;
    }

    retain_test_dir_now(src, label);
}

/// When `DASHD_TEST_RETAIN_DIR` is set, copy `src` to
/// `<DASHD_TEST_RETAIN_DIR>/<test name>/<label>` whatever the test's state, and return that path:
/// for a failure that is returned rather than panicked.
pub fn retain_test_dir_now(src: &Path, label: &str) -> Option<PathBuf> {
    let retain_dir = std::env::var_os("DASHD_TEST_RETAIN_DIR")?;
    let test_name = std::thread::current().name().unwrap_or("unknown").replace(":", "_");
    let dest = PathBuf::from(retain_dir).join(test_name).join(label);
    if dest.exists() {
        let _ = fs::remove_dir_all(&dest);
    }
    if let Err(e) = copy_dir(src, &dest) {
        eprintln!("Failed to retain test data: {}", e);
        None
    } else {
        eprintln!("Test data retained at: {}", dest.display());
        Some(dest)
    }
}

/// Retains node directories if setup panics before the context that would retain them exists,
/// e.g. in a node's start. Declare it before the nodes, so they are signalled first, add each
/// directory, and disarm it once the context is built.
pub(super) struct RetainFailedSetup {
    dirs: Vec<(PathBuf, String)>,
}

impl RetainFailedSetup {
    pub(super) fn new() -> Self {
        Self {
            dirs: Vec::new(),
        }
    }

    pub(super) fn add(&mut self, dir: &Path, label: impl Into<String>) {
        self.dirs.push((dir.to_path_buf(), label.into()));
    }

    pub(super) fn disarm(mut self) {
        self.dirs.clear();
    }
}

impl Drop for RetainFailedSetup {
    fn drop(&mut self) {
        if std::thread::panicking() {
            for (dir, label) in &self.dirs {
                retain_test_dir_now(dir, label);
            }
        }
    }
}
