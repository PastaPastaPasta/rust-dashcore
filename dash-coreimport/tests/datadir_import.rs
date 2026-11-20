//! The import driver on a real filesystem. Every live source is a copy of a
//! checked-in fixture in a 0700 tempdir; no test reads a fixture in place.
use dash_coreimport::datadir_import::{
    FileMetadata, ImportError, ImportFs, ImportOptions, Journal, Method, OsImportFs, Phase,
    PlacementPolicy, SourceGuard, SourceMode, preflight_destination, stage,
};
use dash_coreimport::hash_from_hex;
use dash_coreimport::source::{SourceInspection, inspect};
use std::collections::BTreeMap;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const MAGIC: [u8; 4] = [0xfc, 0xc1, 0xb7, 0xdc];

fn checked_in() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/leveldb/b-l0")
}

fn private_tempdir() -> tempfile::TempDir {
    tempfile::Builder::new().permissions(std::fs::Permissions::from_mode(0o700)).tempdir().unwrap()
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// Path → (bytes, mtime, inode, link count) of every file under `root`.
type Tree = BTreeMap<PathBuf, (Vec<u8>, (i64, i64), u64, u64)>;
fn tree(root: &Path) -> Tree {
    fn walk(root: &Path, dir: &Path, out: &mut Tree) {
        use std::os::unix::fs::MetadataExt;
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                walk(root, &entry.path(), out);
            } else {
                let m = std::fs::symlink_metadata(entry.path()).unwrap();
                out.insert(
                    entry.path().strip_prefix(root).unwrap().to_owned(),
                    (
                        std::fs::read(entry.path()).unwrap(),
                        (m.mtime(), m.mtime_nsec()),
                        m.ino(),
                        m.nlink(),
                    ),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// A 0700 tempdir with a copy of the fixture as the source network dir and
/// an empty destination network dir.
struct Fixture {
    _dir: tempfile::TempDir,
    net: PathBuf,
    dest: PathBuf,
}
fn fixture() -> Fixture {
    let dir = private_tempdir();
    let net = dir.path().join("core");
    copy_tree(&checked_in(), &net);
    let dest = dir.path().join("dest/regtest");
    std::fs::create_dir_all(dest.join("blocks")).unwrap();
    Fixture {
        _dir: dir,
        net,
        dest,
    }
}
fn inspection(net: &Path) -> SourceInspection {
    let expected: serde_json::Value =
        serde_json::from_slice(&std::fs::read(net.join("expected.json")).unwrap()).unwrap();
    let genesis = hash_from_hex(expected["genesis"].as_str().unwrap()).unwrap();
    let source = inspect(net, MAGIC, genesis).unwrap();
    assert_eq!(source.refusal(), None);
    source
}

/// On a real filesystem the journal on disk is durable after each append's
/// fdatasync, so the guard reads it there.
struct Guard {
    journal: PathBuf,
    hardlink: bool,
}
impl SourceGuard for Guard {
    fn release(&self) -> Result<(), ImportError> {
        assert!(!self.hardlink, "hardlink mode released the lock");
        let journal = Journal::open(&self.journal, &OsImportFs::new())?;
        assert!(journal.last() >= Some(Phase::SourceVerified));
        Ok(())
    }
}

/// OsImportFs with FICLONE always failing.
struct NoReflink(OsImportFs);
impl ImportFs for NoReflink {
    fn reflink(&self, _: &Path, _: &Path) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(libc::EOPNOTSUPP))
    }
    fn hard_link(&self, a: &Path, b: &Path) -> io::Result<()> {
        self.0.hard_link(a, b)
    }
    fn copy(&self, a: &Path, b: &Path) -> io::Result<u64> {
        self.0.copy(a, b)
    }
    fn sync_data(&self, p: &Path) -> io::Result<()> {
        self.0.sync_data(p)
    }
    fn sync_dir(&self, p: &Path) -> io::Result<()> {
        self.0.sync_dir(p)
    }
    fn rename(&self, a: &Path, b: &Path) -> io::Result<()> {
        self.0.rename(a, b)
    }
    fn remove(&self, p: &Path) -> io::Result<()> {
        self.0.remove(p)
    }
    fn truncate(&self, p: &Path, l: u64) -> io::Result<()> {
        self.0.truncate(p, l)
    }
    fn append(&self, p: &Path, d: &[u8]) -> io::Result<()> {
        self.0.append(p, d)
    }
    fn metadata(&self, p: &Path) -> io::Result<Option<FileMetadata>> {
        self.0.metadata(p)
    }
    fn read_dir(&self, p: &Path) -> io::Result<Vec<PathBuf>> {
        self.0.read_dir(p)
    }
    fn read(&self, p: &Path) -> io::Result<Vec<u8>> {
        self.0.read(p)
    }
    fn create_dir_all(&self, p: &Path) -> io::Result<()> {
        self.0.create_dir_all(p)
    }
    fn available_space(&self, p: &Path) -> io::Result<u64> {
        self.0.available_space(p)
    }
    fn canonicalize(&self, p: &Path) -> io::Result<PathBuf> {
        self.0.canonicalize(p)
    }
    fn mount_table(&self) -> io::Result<Option<Vec<u8>>> {
        self.0.mount_table()
    }
    fn mount_id(&self, p: &Path) -> io::Result<Option<u64>> {
        self.0.mount_id(p)
    }
    fn sha256(&self, p: &Path) -> io::Result<[u8; 32]> {
        self.0.sha256(p)
    }
}

fn import(fx: &Fixture, options: ImportOptions, fs: &dyn ImportFs) -> Result<Phase, ImportError> {
    preflight_destination(&fx.dest, &fx.net, fs)?;
    let guard = Guard {
        journal: fx.dest.join("import-staging/JOURNAL"),
        hardlink: options.placement == PlacementPolicy::Hardlink,
    };
    stage(&inspection(&fx.net), options, &fx.net, None, &fx.dest, fs, &guard)
}

#[test]
fn stage_on_fixture_copy_leaves_it_byte_identical() {
    let original = tree(&checked_in());
    for (options, no_reflink) in [
        (
            ImportOptions {
                mode: SourceMode::Lock,
                placement: PlacementPolicy::Reflink,
            },
            false,
        ),
        (
            ImportOptions {
                mode: SourceMode::Lock,
                placement: PlacementPolicy::Reflink,
            },
            true,
        ),
        (
            ImportOptions {
                mode: SourceMode::Snapshot,
                placement: PlacementPolicy::Reflink,
            },
            true,
        ),
        (
            ImportOptions {
                mode: SourceMode::Lock,
                placement: PlacementPolicy::Hardlink,
            },
            true,
        ),
    ] {
        let fx = fixture();
        let before = tree(&fx.net);
        let fs: Box<dyn ImportFs> = if no_reflink {
            Box::new(NoReflink(OsImportFs::new()))
        } else {
            Box::new(OsImportFs::new())
        };
        assert_eq!(import(&fx, options, fs.as_ref()).unwrap(), Phase::Placed);
        assert_eq!(tree(&fx.net), before, "{options:?}");
        assert_eq!(
            std::fs::read(fx.dest.join("blocks/blk00000.dat")).unwrap(),
            original[Path::new("blocks/blk00000.dat")].0
        );
        let journal = Journal::open(&fx.dest.join("import-staging/JOURNAL"), fs.as_ref()).unwrap();
        // 'l' is 0 in this fixture, so even hardlink mode copies.
        let method = journal.methods().unwrap()[0].method;
        assert!(!no_reflink || method == Method::Copy, "{method:?}");
    }
    assert_eq!(tree(&checked_in()), original);
}

#[test]
fn same_source_relative_or_symlinked_spelling_resumes() {
    let fx = fixture();
    let options = ImportOptions {
        mode: SourceMode::Lock,
        placement: PlacementPolicy::Reflink,
    };
    let fs = OsImportFs::new();
    // A first run that stops at SourceVerified: the source guard double fails the
    // release, after SourceVerified is durable, and P3 never runs.
    struct Refuse;
    impl SourceGuard for Refuse {
        fn release(&self) -> Result<(), ImportError> {
            Err(ImportError::LockUnavailable)
        }
    }
    assert!(stage(&inspection(&fx.net), options, &fx.net, None, &fx.dest, &fs, &Refuse).is_err());
    let link = fx._dir.path().join("link");
    std::os::unix::fs::symlink(&fx.net, &link).unwrap();
    let cwd = std::env::current_dir().unwrap();
    let up = "../".repeat(cwd.components().count() - 1);
    let relative = PathBuf::from(up).join(fx.net.strip_prefix("/").unwrap());
    let trailing = PathBuf::from(format!("{}/", fx.net.display()));
    for spelling in [&link, &relative, &trailing] {
        let j = preflight_destination(&fx.dest, spelling, &fs).unwrap().unwrap();
        assert_eq!(j.last(), Some(Phase::SourceVerified), "{}", spelling.display());
    }
    let mut src = inspection(&fx.net);
    src.net_dir = relative;
    let guard = Guard {
        journal: fx.dest.join("import-staging/JOURNAL"),
        hardlink: false,
    };
    assert_eq!(stage(&src, options, &fx.net, None, &fx.dest, &fs, &guard).unwrap(), Phase::Placed);
}

#[test]
fn nesting_through_a_symlink_is_refused() {
    let fx = fixture();
    let link = fx._dir.path().join("link");
    std::os::unix::fs::symlink(&fx.net, &link).unwrap();
    let fs = OsImportFs::new();
    for dest in [link.join("dest"), fx.net.join("blocks"), fx._dir.path().to_owned()] {
        assert!(
            matches!(preflight_destination(&dest, &fx.net, &fs), Err(ImportError::Overlap { .. })),
            "{}",
            dest.display()
        );
    }
    assert!(!fx.net.join("dest").exists());
}

#[test]
fn a_replaced_source_dir_is_refused() {
    let fx = fixture();
    let fs = OsImportFs::new();
    assert_eq!(
        import(
            &fx,
            ImportOptions {
                mode: SourceMode::Lock,
                placement: PlacementPolicy::Reflink
            },
            &fs
        )
        .unwrap(),
        Phase::Placed
    );
    std::fs::rename(&fx.net, fx._dir.path().join("old")).unwrap();
    copy_tree(&fx._dir.path().join("old"), &fx.net);
    assert!(matches!(
        preflight_destination(&fx.dest, &fx.net, &fs),
        Err(ImportError::DestinationNotEmpty(p)) if p.ends_with("import-staging")
    ));
}

/// CutFs is usable from integration tests with the `test-hooks` feature.
#[cfg(feature = "test-hooks")]
#[test]
fn cutfs_runs_stage_from_integration_tests() {
    use dash_coreimport::datadir_import::cutfs::CutFs;
    let net = Path::new("/src/regtest");
    let fs = CutFs::image()
        .with_file(net.join("blocks/blk00000.dat"), b"block zero")
        .with_file(net.join("chainstate/CURRENT"), b"x")
        .with_dir("/dest/regtest/blocks")
        .build();
    let src = SourceInspection::from_parts(
        net.to_owned(),
        vec![(0, net.join("blocks/blk00000.dat"))],
        Some([1; 32]),
        Some(0),
    );
    struct Ok_;
    impl SourceGuard for Ok_ {
        fn release(&self) -> Result<(), ImportError> {
            Ok(())
        }
    }
    let options = ImportOptions {
        mode: SourceMode::Lock,
        placement: PlacementPolicy::Reflink,
    };
    let dest = Path::new("/dest/regtest");
    preflight_destination(dest, net, &fs).unwrap();
    assert_eq!(
        stage(&src, options, Path::new("/src"), Some("regtest"), dest, &fs, &Ok_).unwrap(),
        Phase::Placed
    );
    fs.crash();
    assert_eq!(fs.read(&dest.join("blocks/blk00000.dat")).unwrap(), b"block zero");
}
