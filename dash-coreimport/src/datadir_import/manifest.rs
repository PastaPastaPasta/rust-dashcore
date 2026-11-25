//! Source manifests (§2.4).
use super::fs::ImportFs;
use super::journal::Reader;
use bitcoin_hashes::sha256;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// The trees under the network dir that the manifest covers.
pub const SCOPED_DIRS: [&str; 5] = ["blocks", "chainstate", "evodb", "llmq", "indexes"];
/// The smallest encoded entry: an empty path and the fixed fields.
const MIN_ENTRY: usize = 4 + 8 + 16 + 8 + 8 + 32 + 8;
/// Linux's `PATH_MAX`: the longest path a manifest entry or record carries.
pub(crate) const MAX_PATH: usize = 4096;
/// The most entries a manifest may hold.
pub(crate) const MAX_ENTRIES: usize = 1_000_000;
/// The longest encoded manifest: count, the entries and the digest.
pub(crate) const MAX_ENCODED: usize = 4 + MAX_ENTRIES * (MIN_ENTRY + MAX_PATH) + 32;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestEntry {
    /// Relative to the network dir.
    pub path: PathBuf,
    pub size: u64,
    pub modified: u128,
    pub dev: u64,
    pub ino: u64,
    /// sha256 of the content; zero for `.lock` and top-level symlinks, which
    /// are recorded by `symlink_metadata` only.
    pub hash: [u8; 32],
    pub nlink: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceManifest {
    pub entries: Vec<ManifestEntry>,
    pub digest: [u8; 32],
}

fn encode_entries(entries: &[ManifestEntry]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for e in entries {
        let path = e.path.as_os_str().as_bytes();
        out.extend_from_slice(&(path.len() as u32).to_le_bytes());
        out.extend_from_slice(path);
        out.extend_from_slice(&e.size.to_le_bytes());
        out.extend_from_slice(&e.modified.to_le_bytes());
        out.extend_from_slice(&e.dev.to_le_bytes());
        out.extend_from_slice(&e.ino.to_le_bytes());
        out.extend_from_slice(&e.hash);
        out.extend_from_slice(&e.nlink.to_le_bytes());
    }
    out
}

fn vanished(path: &Path) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, format!("{} vanished during the scan", path.display()))
}

fn symlinked(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "{} is a symlink; symlinked source dirs and files are not supported",
            path.display()
        ),
    )
}

fn relative(path: &Path, base: &Path) -> io::Result<PathBuf> {
    path.strip_prefix(base)
        .map(Path::to_owned)
        .map_err(|_| io::Error::other(format!("{} is outside {}", path.display(), base.display())))
}

impl SourceManifest {
    /// Scans the §2.4 scope of a source network dir.
    pub fn scan(net: &Path, fs: &dyn ImportFs) -> io::Result<Self> {
        fn walk(
            dir: &Path,
            base: &Path,
            fs: &dyn ImportFs,
            out: &mut Vec<PathBuf>,
        ) -> io::Result<()> {
            for x in fs.read_dir(dir)? {
                let m = fs.metadata(&x)?.ok_or_else(|| vanished(&x))?;
                if m.is_symlink {
                    return Err(symlinked(&x));
                }
                if m.is_dir {
                    walk(&x, base, fs, out)?;
                } else {
                    out.push(relative(&x, base)?);
                }
            }
            Ok(())
        }
        let mut paths = Vec::new();
        for x in fs.read_dir(net)? {
            let m = fs.metadata(&x)?.ok_or_else(|| vanished(&x))?;
            let scoped =
                x.file_name().and_then(OsStr::to_str).is_some_and(|n| SCOPED_DIRS.contains(&n));
            if scoped && m.is_symlink {
                return Err(symlinked(&x));
            }
            if m.is_dir {
                if scoped {
                    walk(&x, net, fs, &mut paths)?;
                }
            } else {
                paths.push(relative(&x, net)?);
            }
        }
        paths.sort();
        if paths.len() > MAX_ENTRIES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} holds more than {MAX_ENTRIES} files", net.display()),
            ));
        }
        if let Some(long) = paths.iter().find(|p| p.as_os_str().len() > MAX_PATH) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is longer than {MAX_PATH} bytes", long.display()),
            ));
        }
        let mut entries = Vec::with_capacity(paths.len());
        for rel in paths {
            let p = net.join(&rel);
            let m = fs.metadata(&p)?.ok_or_else(|| vanished(&p))?;
            let metadata_only = m.is_symlink || rel.file_name().is_some_and(|x| x == ".lock");
            let hash = if metadata_only {
                [0; 32]
            } else {
                fs.sha256(&p)?
            };
            entries.push(ManifestEntry {
                path: rel,
                size: m.len,
                modified: m.modified,
                dev: m.dev,
                ino: m.ino,
                hash,
                nlink: m.nlink,
            });
        }
        Ok(Self::from_entries(entries))
    }

    /// A manifest of `entries`, sorted by path.
    pub fn from_entries(mut entries: Vec<ManifestEntry>) -> Self {
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        Self {
            digest: sha256::Hash::hash(&encode_entries(&entries)).to_byte_array(),
            entries,
        }
    }

    pub fn get(&self, rel: &Path) -> Option<&ManifestEntry> {
        let i = self.entries.binary_search_by(|e| e.path.as_path().cmp(rel)).ok()?;
        Some(&self.entries[i])
    }

    /// The `blocks/blk*.dat` entries, keyed by file name.
    pub fn blk_entries(&self) -> impl Iterator<Item = &ManifestEntry> {
        self.entries.iter().filter(|e| {
            e.path.parent() == Some(Path::new("blocks"))
                && e.path.file_name().and_then(OsStr::to_str).is_some_and(|n| n.starts_with("blk"))
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = encode_entries(&self.entries);
        out.extend_from_slice(&self.digest);
        out
    }

    /// Decodes an encoded manifest and checks its digest and that its
    /// entries are strictly sorted by path. Allocation is bounded by the
    /// input length.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let mut r = Reader {
            b: bytes,
            p: 0,
        };
        let count = r.u32()? as usize;
        if count > MAX_ENTRIES || count > bytes.len() / MIN_ENTRY {
            return None;
        }
        let mut entries: Vec<ManifestEntry> = Vec::with_capacity(count);
        for _ in 0..count {
            let path = PathBuf::from(OsStr::from_bytes(r.bytes_max(MAX_PATH)?));
            if entries.last().is_some_and(|prev| prev.path >= path) {
                return None;
            }
            entries.push(ManifestEntry {
                path,
                size: r.u64()?,
                modified: r.u128()?,
                dev: r.u64()?,
                ino: r.u64()?,
                hash: r.take(32)?.try_into().ok()?,
                nlink: r.u64()?,
            });
        }
        let digest: [u8; 32] = r.take(32)?.try_into().ok()?;
        if r.p != bytes.len() {
            return None;
        }
        let manifest = Self::from_entries(entries);
        (manifest.digest == digest).then_some(manifest)
    }

    /// P2's check (M1 against M0): everything equal, and `nlink` exactly
    /// `M0.nlink + 1` for each file the import hardlinked, unchanged otherwise.
    pub fn check_placement(
        &self,
        later: &Self,
        linked: &BTreeSet<PathBuf>,
    ) -> Result<(), Vec<PathBuf>> {
        self.compare(later, |a| a.nlink + u64::from(linked.contains(&a.path)))
    }

    /// Later checks: the expected `nlink` is `M0.nlink` plus the number of
    /// destination blk files that share the source file's `(st_dev, st_ino)`.
    pub fn check_live(
        &self,
        later: &Self,
        dest_blocks: &Path,
        fs: &dyn ImportFs,
    ) -> Result<(), Vec<PathBuf>> {
        let mut live: BTreeMap<(u64, u64), u64> = BTreeMap::new();
        for m in fs
            .read_dir(dest_blocks)
            .unwrap_or_default()
            .iter()
            .filter_map(|p| fs.metadata(p).ok().flatten())
            .filter(|m| !m.is_dir)
        {
            *live.entry(m.id()).or_default() += 1;
        }
        self.compare(later, |a| a.nlink + live.get(&(a.dev, a.ino)).copied().unwrap_or(0))
    }

    fn compare(
        &self,
        later: &Self,
        expected_nlink: impl Fn(&ManifestEntry) -> u64,
    ) -> Result<(), Vec<PathBuf>> {
        // Both entry lists are sorted by path, so one merge pass compares them.
        let mut bad = Vec::new();
        let (mut xs, mut ys) = (self.entries.iter().peekable(), later.entries.iter().peekable());
        loop {
            let order = match (xs.peek(), ys.peek()) {
                (None, None) => break,
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (Some(a), Some(b)) => a.path.cmp(&b.path),
            };
            match order {
                Ordering::Less => bad.extend(xs.next().map(|a| a.path.clone())),
                Ordering::Greater => bad.extend(ys.next().map(|b| b.path.clone())),
                Ordering::Equal => {
                    if let (Some(a), Some(b)) = (xs.next(), ys.next())
                        && !(a.size == b.size
                            && a.modified == b.modified
                            && a.dev == b.dev
                            && a.ino == b.ino
                            && a.hash == b.hash
                            && b.nlink == expected_nlink(a))
                    {
                        bad.push(a.path.clone());
                    }
                }
            }
        }
        if bad.is_empty() {
            Ok(())
        } else {
            Err(bad)
        }
    }
}
