//! Owner-only test directories, retained for post-mortems like dash-spv's.

use std::io;
use std::path::{Path, PathBuf};

use dash_spv::test_utils::{retain_test_dir, retain_test_dir_now};

/// A temporary directory, mode 0700 (whatever the umask), removed on drop.
///
/// When the test is panicking, or `DASHD_TEST_RETAIN_ALWAYS=1`, it is first copied to
/// `$DASHD_TEST_RETAIN_DIR/<test name>/<label>` (dash-spv's [`retain_test_dir`]). Drop the nodes
/// using it first, so their logs are complete.
#[derive(Debug)]
pub struct TestDir {
    dir: tempfile::TempDir,
    label: String,
}

impl TestDir {
    /// A new directory under the system temporary directory (`TMPDIR`).
    pub fn new(label: &str) -> io::Result<Self> {
        let dir = tempfile::Builder::new().prefix(&format!("dash-testkit-{label}-")).tempdir()?;
        create_private_dir(dir.path())?;
        Ok(Self {
            dir,
            label: label.to_string(),
        })
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Copies the directory to `$DASHD_TEST_RETAIN_DIR/<test name>/<label>` now, whatever the
    /// test's state, and returns where; for a failure returned rather than panicked.
    pub fn retain(&self) -> Option<PathBuf> {
        retain_test_dir_now(self.dir.path(), &self.label)
    }

    /// The owner-only subdirectory `name`, created if missing.
    pub fn subdir(&self, name: &str) -> io::Result<PathBuf> {
        let path = self.dir.path().join(name);
        create_private_dir(&path)?;
        Ok(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        retain_test_dir(self.dir.path(), &self.label);
    }
}

/// Creates `path` and its missing parents with mode 0700 (the box's umask would give 0775), and
/// makes `path` itself 0700 if it already existed.
pub(crate) fn create_private_dir(path: &Path) -> io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(path)?;
    #[cfg(unix)]
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    Ok(())
}
