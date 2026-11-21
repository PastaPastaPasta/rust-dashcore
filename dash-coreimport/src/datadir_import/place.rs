//! P1 file placement (§2.2).
use super::fs::ImportFs;
use super::journal::{FileMethod, Method};
use super::manifest::SourceManifest;
use super::{ImportError, ImportOptions, PlacementPolicy, SourceMode};
use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

/// rdashd's rev files, as a share of the blk bytes (§2.2: about 12 %).
const REV_HEADROOM_PERCENT: u64 = 12;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Placement {
    pub methods: Vec<FileMethod>,
    /// The M0 paths (`blocks/blk…`) the import hardlinked.
    pub linked: BTreeSet<PathBuf>,
}

/// The blk files to place: `(n, source path)`, in order.
pub type BlkFiles<'a> = &'a [(u32, PathBuf)];

/// The FICLONE errors that fall back to a hardlink or copy (§2.2); any other
/// error stops P1.
const REFLINK_FALLBACK: [i32; 3] = [libc::EOPNOTSUPP, libc::EXDEV, libc::EINVAL];

fn errno(e: &io::Error) -> i32 {
    e.raw_os_error().unwrap_or(-1)
}

/// The free space P1 needs: every blk byte (any FICLONE may fail), minus
/// the files below `'l'` under `Hardlink`, plus rev headroom on every blk
/// byte and dashd-store's `MIN_DISK_SPACE`.
pub fn required_space(sizes: &[(u32, u64)], options: ImportOptions, last_file: Option<u32>) -> u64 {
    let total: u64 = sizes.iter().map(|(_, s)| *s).sum();
    let placed: u64 =
        sizes.iter().filter(|(n, _)| !linkable(*n, options, last_file)).map(|(_, s)| *s).sum();
    placed
        .saturating_add(total / 100 * REV_HEADROOM_PERCENT)
        .saturating_add(dashd_store::flatfile::MIN_DISK_SPACE)
}

fn linkable(n: u32, options: ImportOptions, last_file: Option<u32>) -> bool {
    options.mode == SourceMode::Lock
        && options.placement == PlacementPolicy::Hardlink
        && last_file.is_some_and(|l| n < l)
}

/// Removes `d` if it exists, refusing first if it is the source's inode.
fn clear_destination(src: &Path, d: &Path, fs: &dyn ImportFs) -> Result<(), ImportError> {
    let Some(dm) = fs.metadata(d)? else {
        return Ok(());
    };
    if fs.metadata(src)?.is_some_and(|sm| sm.id() == dm.id()) {
        return Err(ImportError::StagingAliasesSource(d.to_owned()));
    }
    fs.remove(d)?;
    Ok(())
}

/// Places each blk file in `<dest>/import-staging/blocks` and `fdatasync`s
/// it; every copy is re-read and compared with M0. A FICLONE failure other
/// than `EOPNOTSUPP`, `EXDEV` or `EINVAL` is returned, with no fallback. No path that exists is
/// ever opened for writing: each staged file is created new.
pub fn place(
    files: BlkFiles<'_>,
    last_file: Option<u32>,
    options: ImportOptions,
    m0: &SourceManifest,
    dest: &Path,
    fs: &dyn ImportFs,
) -> Result<Placement, ImportError> {
    options.validate()?;
    let blocks = dest.join("import-staging/blocks");
    let mut sizes = Vec::with_capacity(files.len());
    for (n, p) in files {
        let m = fs.metadata(p)?.ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, format!("{} vanished", p.display()))
        })?;
        sizes.push((*n, m.len));
    }
    let required = required_space(&sizes, options, last_file);
    let available = fs.available_space(dest)?;
    if available < required {
        return Err(ImportError::InsufficientSpace {
            required,
            available,
        });
    }
    fs.create_dir_all(&blocks)?;
    let mut out = Placement::default();
    for (n, p) in files {
        let name = p.file_name().ok_or_else(|| ImportError::StagedCopyMismatch {
            path: p.clone(),
        })?;
        let rel = Path::new("blocks").join(name);
        let entry = m0.get(&rel).ok_or_else(|| ImportError::StagedCopyMismatch {
            path: rel.clone(),
        })?;
        let d = blocks.join(name);
        clear_destination(p, &d, fs)?;
        let mut row = FileMethod {
            name: PathBuf::from(name),
            method: Method::Reflink,
            reflink_errno: 0,
            link_errno: 0,
        };
        if let Err(e) = fs.reflink(p, &d) {
            if !REFLINK_FALLBACK.contains(&errno(&e)) {
                return Err(ImportError::Io(e));
            }
            row.reflink_errno = errno(&e);
            clear_destination(p, &d, fs)?;
            row.method = Method::Copy;
            if linkable(*n, options, last_file) {
                match fs.hard_link(p, &d) {
                    Ok(()) => row.method = Method::Hardlink,
                    Err(e) => {
                        row.link_errno = errno(&e);
                        clear_destination(p, &d, fs)?;
                    }
                }
            }
            if row.method == Method::Copy {
                fs.copy(p, &d)?;
            }
        }
        match row.method {
            Method::Hardlink => {
                out.linked.insert(rel.clone());
            }
            Method::Reflink | Method::Copy => fs.sync_data(&d)?,
        }
        if row.method == Method::Copy && fs.sha256(&d)? != entry.hash {
            return Err(ImportError::StagedCopyMismatch {
                path: rel,
            });
        }
        out.methods.push(row);
    }
    Ok(out)
}
