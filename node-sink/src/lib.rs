// EMBARGO-1
//! Test infrastructure for the node program's private test data (PLAN §7 handling, CS0 §9.3–§9.5).
//!
//! - The private tree is `$DASH_EMBARGO_DIR`. Only the manager's integration gate (and designated
//!   readers) set it; ordinary runs leave it unset, and the tests that need it skip loudly.
//! - Everything this crate writes to the private tree is created with modes 0700 (directories)
//!   and 0600 (files), whatever the umask. Those modes need Unix: elsewhere (Windows) every
//!   private operation refuses ([`PRIVATE_FILES_SUPPORTED`]), and a private test fails when the
//!   variable is set. The manifests, reports and the synthetic-corpus writers are portable.
//! - Nothing here prints input bytes or `Debug` of values derived from inputs: reports name a
//!   test, a case index or a field, and the sink's opaque ids.
//!
//! Modules: [`manifest`] (the private corpora's manifests and synthetic-corpus writers for
//! tests), [`sink`] (divergence records, the fuzz targets' start-up check and the opaque
//! proptest seed).

#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::use_debug,
    clippy::dbg_macro,
    clippy::print_stdout
)]

pub mod manifest;
pub mod sink;

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

pub use private_fs::{create_private_dir, is_private_dir, write_private_file};

/// The private tree's root.
pub const DIR_VAR: &str = "DASH_EMBARGO_DIR";
/// The gate's run id, which receipts carry.
pub const RECEIPT_ID_VAR: &str = "DASH_EMBARGO_RECEIPT_ID";

/// Whether this platform can hold the private tree: its modes (0700, 0600) need Unix. Where it
/// cannot, [`create_private_dir`] and [`write_private_file`] fail with
/// [`io::ErrorKind::Unsupported`] and [`is_private_dir`] is false; nothing falls back to
/// ordinary files.
pub const PRIVATE_FILES_SUPPORTED: bool = cfg!(unix);

/// The private tree, or `None` when [`DIR_VAR`] is unset or empty.
pub fn embargo_dir() -> Option<PathBuf> {
    std::env::var_os(DIR_VAR).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// The private tree for the private test `test`. When [`DIR_VAR`] is unset it prints the skip
/// line ([`report_skip`]) and returns `None`, and the test passes.
///
/// # Panics
///
/// When [`DIR_VAR`] is set to a relative path (each test binary runs in its own crate's
/// directory, so it would name a different tree in each), or on a platform that cannot hold the
/// private tree ([`PRIVATE_FILES_SUPPORTED`]): the test fails before it reads anything private.
pub fn private_tree(test: &str) -> Option<PathBuf> {
    let Some(root) = embargo_dir() else {
        report_skip(test);
        return None;
    };
    if !root.is_absolute() {
        report(&format!("{test}: FAIL"));
        panic!("{test}: {DIR_VAR} must be an absolute path");
    }
    if !PRIVATE_FILES_SUPPORTED {
        report(&format!("{test}: FAIL"));
        panic!("{test}: {DIR_VAR} is set, but this platform cannot keep private files (Unix only)");
    }
    Some(root)
}

/// Writes one line to stderr directly. libtest captures `eprintln!` from passing tests, so a
/// skip printed that way would not be seen (CS0 §9.4).
pub fn report(line: &str) {
    let mut stderr = io::stderr().lock();
    let _ = stderr.write_all(line.as_bytes());
    let _ = stderr.write_all(b"\n");
}

/// The fixed line a private test prints when [`DIR_VAR`] is unset; the test then passes.
pub fn report_skip(test: &str) {
    report(&format!("{test}: SKIPPED: {DIR_VAR} is unset; the integration gate runs this test"));
}

#[cfg(unix)]
mod private_fs {
    use std::fs::{DirBuilder, OpenOptions};
    use std::io::{self, Write as _};
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
    use std::path::Path;

    /// Creates `path` and its missing parents with mode 0700.
    pub fn create_private_dir(path: &Path) -> io::Result<()> {
        DirBuilder::new().recursive(true).mode(0o700).create(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
    }

    /// Writes `bytes` to `path` (created or truncated) with mode 0600.
    pub fn write_private_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
        let mut file =
            OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.write_all(bytes)
    }

    /// Whether `path` is a directory that only its owner can read.
    pub fn is_private_dir(path: &Path) -> bool {
        std::fs::metadata(path).is_ok_and(|m| m.is_dir() && m.permissions().mode() & 0o077 == 0)
    }
}

/// Without Unix modes every private operation refuses ([`crate::PRIVATE_FILES_SUPPORTED`]).
#[cfg(not(unix))]
mod private_fs {
    use std::io;
    use std::path::Path;

    fn unsupported() -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "private files need Unix modes 0700/0600; this platform refuses them",
        )
    }

    /// Refuses: a private directory needs mode 0700.
    pub fn create_private_dir(_path: &Path) -> io::Result<()> {
        Err(unsupported())
    }

    /// Refuses: a private file needs mode 0600.
    pub fn write_private_file(_path: &Path, _bytes: &[u8]) -> io::Result<()> {
        Err(unsupported())
    }

    /// False: no directory is known to be private here.
    pub fn is_private_dir(_path: &Path) -> bool {
        false
    }
}

/// Lower-case hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    dash_sha256::sha256(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Writes the receipt of a successful private test, with the gate's run id ([`RECEIPT_ID_VAR`],
/// `-` when unset) and the counts, one `key value` per line. The gate checks the run id and the
/// counts (CS0 §9.4). The receipt is `.receipts/<run id>/<test>` in the private tree, so gates
/// running at once never overwrite each other's; without a run id it is `.receipts/<test>`.
pub fn write_receipt(dir: &Path, test: &str, counts: &[(&str, u64)]) -> io::Result<()> {
    // Only an unset variable means "no run id"; an empty or non-UTF-8 one is a mistake to refuse.
    let id = match std::env::var(RECEIPT_ID_VAR) {
        Ok(id) => Some(id),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{RECEIPT_ID_VAR} must be UTF-8"),
            ));
        }
    };
    write_receipt_for(dir, test, id.as_deref(), counts)
}

fn write_receipt_for(
    dir: &Path,
    test: &str,
    id: Option<&str>,
    counts: &[(&str, u64)],
) -> io::Result<()> {
    let mut receipts = dir.join(".receipts");
    if let Some(id) = id {
        // The id names a directory, so it must be exactly one plain path component: `components`
        // alone would accept `run/` and `run/.` as `run`.
        let mut parts = Path::new(id).components();
        let single = matches!(
            (parts.next(), parts.next()),
            (Some(std::path::Component::Normal(name)), None) if name == id
        );
        if !single {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{RECEIPT_ID_VAR} must be a single path component"),
            ));
        }
        receipts.push(id);
    }
    create_private_dir(&receipts)?;
    let mut body = format!("run {}\n", id.unwrap_or("-"));
    for (key, value) in counts {
        body.push_str(&format!("{key} {value}\n"));
    }
    write_private_file(&receipts.join(test), body.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/tmp/node-sink").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[cfg(unix)]
    #[test]
    fn private_writes_get_private_modes() -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = scratch("modes").join("a/b");
        create_private_dir(&dir)?;
        assert!(is_private_dir(&dir));
        let file = dir.join("f");
        write_private_file(&file, b"x")?;
        assert_eq!(std::fs::metadata(&file)?.permissions().mode() & 0o777, 0o600);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn receipts_carry_the_counts() -> io::Result<()> {
        let dir = scratch("receipt");
        write_receipt(&dir, "t", &[("cases", 3)])?;
        let body = std::fs::read_to_string(dir.join(".receipts/t"))?;
        assert!(body.ends_with("cases 3\n"));
        assert!(body.starts_with("run "));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn receipts_of_a_run_id_get_their_own_directory() -> io::Result<()> {
        let dir = scratch("receipt-run");
        write_receipt_for(&dir, "t", Some("run-a"), &[("cases", 3)])?;
        write_receipt_for(&dir, "t", Some("run-b"), &[("cases", 4)])?;
        let read = |run: &str| std::fs::read_to_string(dir.join(".receipts").join(run).join("t"));
        assert_eq!(read("run-a")?, "run run-a\ncases 3\n");
        assert_eq!(read("run-b")?, "run run-b\ncases 4\n");
        assert!(!dir.join(".receipts/t").exists());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn a_run_id_must_be_one_path_component() {
        let dir = scratch("receipt-bad-id");
        for id in ["", "..", ".", "a/b", "/abs", "../x", "run-a/", "run-a//", "run-a/."] {
            let written = write_receipt_for(&dir, "t", Some(id), &[("cases", 1)]);
            assert!(written.is_err_and(|e| e.kind() == io::ErrorKind::InvalidInput), "{id}");
        }
        assert!(!dir.exists());
    }

    /// `write_receipt` reads the run id from the environment. The variable is process-global, so
    /// the test runs `write_receipt` in a child copy of this test binary (the child branch below).
    #[cfg(unix)]
    #[test]
    fn write_receipt_reads_the_run_id_from_the_environment() -> io::Result<()> {
        const CHILD_DIR: &str = "NODE_SINK_TEST_RECEIPT_DIR";
        if let Some(dir) = std::env::var_os(CHILD_DIR) {
            return write_receipt(Path::new(&dir), "t", &[("cases", 1)]);
        }
        let dir = scratch("receipt-env");
        let child_succeeds = |id: Option<&str>| -> io::Result<bool> {
            let mut child = std::process::Command::new(std::env::current_exe()?);
            child
                .args(["--exact", "tests::write_receipt_reads_the_run_id_from_the_environment"])
                .env(CHILD_DIR, &dir)
                .env_remove(RECEIPT_ID_VAR);
            if let Some(id) = id {
                child.env(RECEIPT_ID_VAR, id);
            }
            Ok(child.output()?.status.success())
        };
        for bad in ["", "run-a/", "run-a//", "run-a/.", "..", "a/b"] {
            assert!(!child_succeeds(Some(bad))?, "{bad}");
        }
        assert!(!dir.exists());
        assert!(child_succeeds(Some("run-a"))?);
        assert_eq!(std::fs::read_to_string(dir.join(".receipts/run-a/t"))?, "run run-a\ncases 1\n");
        assert!(child_succeeds(None)?);
        assert_eq!(std::fs::read_to_string(dir.join(".receipts/t"))?, "run -\ncases 1\n");
        Ok(())
    }

    #[cfg(not(unix))]
    #[test]
    fn private_operations_refuse_without_unix_modes() {
        let dir = scratch("refuse");
        let refused = |r: io::Result<()>| r.is_err_and(|e| e.kind() == io::ErrorKind::Unsupported);
        assert!(refused(create_private_dir(&dir)));
        assert!(refused(write_private_file(&dir.join("f"), b"x")));
        assert!(refused(write_receipt(&dir, "t", &[("cases", 3)])));
        assert!(!is_private_dir(&dir));
        assert!(!dir.exists());
    }

    #[test]
    fn sha256_hex_is_lowercase_hex() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
