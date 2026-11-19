//! The mount table that [`super::check_exclusion`] reads, in Linux
//! `/proc/self/mountinfo` format.
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

/// Where [`super::OsImportFs`] reads the mount table on Linux.
pub const MOUNTINFO: &str = "/proc/self/mountinfo";

/// What every "no mount holds it" error adds: why a readable table can lack
/// the mount and what to do.
const CHROOT_HINT: &str = "the kernel lists no mount whose mount point lies outside the reader's root, so in a chroot whose root is not a mount point the table is empty or lacks it; run the node outside the chroot, or bind-mount the chroot root onto itself";

/// One mountinfo line: the mount id, the filesystem's `major:minor`, the root
/// of the mount inside that filesystem, and where it is mounted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Mount {
    pub id: u64,
    pub dev: (u32, u32),
    pub root: PathBuf,
    pub mount_point: PathBuf,
}

/// Parses mountinfo text. Fields are space-separated; the optional fields
/// after the sixth run up to a lone `-`, which three more fields follow. Paths
/// carry `\ooo` octal escapes for space, tab, newline and backslash.
pub fn parse(text: &[u8]) -> Result<Vec<Mount>, String> {
    let mut out = Vec::new();
    for (i, line) in text.split(|&b| b == b'\n').enumerate() {
        if line.is_empty() {
            continue;
        }
        let bad = |what: &str| format!("line {}: {what}", i + 1);
        let fields: Vec<&[u8]> = line.split(|&b| b == b' ').collect();
        let sep = fields
            .iter()
            .skip(6)
            .position(|f| *f == b"-")
            .map(|p| p + 6)
            .ok_or_else(|| bad("no `-` after the optional fields"))?;
        if fields.len() < sep + 4 {
            return Err(bad("fewer than three fields after `-`"));
        }
        let id = number::<u64>(fields[0])
            .filter(|_| number::<u64>(fields[1]).is_some())
            .ok_or_else(|| bad("bad mount id"))?;
        let dev = std::str::from_utf8(fields[2])
            .ok()
            .and_then(|f| f.split_once(':'))
            .and_then(|(a, b)| Some((number(a.as_bytes())?, number(b.as_bytes())?)))
            .ok_or_else(|| bad("bad major:minor"))?;
        let root = unescape(fields[3]).ok_or_else(|| bad("bad escape in the root"))?;
        let mount_point =
            unescape(fields[4]).ok_or_else(|| bad("bad escape in the mount point"))?;
        if !mount_point.is_absolute() {
            return Err(bad("the mount point is not absolute"));
        }
        out.push(Mount {
            id,
            dev,
            root,
            mount_point,
        });
    }
    if out.is_empty() {
        return Err(format!("no mounts: {CHROOT_HINT}"));
    }
    Ok(out)
}

fn number<T: std::str::FromStr>(f: &[u8]) -> Option<T> {
    if f.is_empty() || !f.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(f).ok()?.parse().ok()
}

fn unescape(f: &[u8]) -> Option<PathBuf> {
    let mut out = Vec::with_capacity(f.len());
    let mut i = 0;
    while i < f.len() {
        if f[i] == b'\\' {
            let digits = f.get(i + 1..i + 4)?;
            if !digits.iter().all(|d| (b'0'..=b'7').contains(d)) {
                return None;
            }
            let v = digits.iter().fold(0u32, |v, d| v * 8 + u32::from(d - b'0'));
            out.push(u8::try_from(v).ok()?);
            i += 4;
        } else {
            out.push(f[i]);
            i += 1;
        }
    }
    Some(PathBuf::from(OsString::from_vec(out)))
}

/// The mount holding `path` and `path`'s path inside that mount's
/// filesystem. With `id` (the kernel's mount id for `path`, from `statx`) it
/// is the entry with that id, which must exist; without one, the entry whose
/// mount point is the longest component-wise prefix of `path` (a later entry,
/// mounted on top, wins a tie).
pub fn locate<'a>(
    mounts: &'a [Mount],
    path: &Path,
    id: Option<u64>,
) -> Result<(&'a Mount, PathBuf), String> {
    let m = match id {
        Some(id) => mounts.iter().find(|m| m.id == id).ok_or_else(|| {
            format!("the mount id {id} of {} is not in the table: {CHROOT_HINT}", path.display())
        })?,
        None => {
            let depth = |m: &Mount| m.mount_point.components().count();
            mounts
                .iter()
                .filter(|m| path.starts_with(&m.mount_point))
                .fold(None, |best: Option<&Mount>, m| match best {
                    Some(b) if depth(b) > depth(m) => Some(b),
                    _ => Some(m),
                })
                .ok_or_else(|| format!("no mount holds {}: {CHROOT_HINT}", path.display()))?
        }
    };
    let rest = path.strip_prefix(&m.mount_point).map_err(|_| {
        format!(
            "the mount holding {} is at {}, which is not above it: {CHROOT_HINT}",
            path.display(),
            m.mount_point.display()
        )
    })?;
    let inside = if rest.as_os_str().is_empty() {
        m.root.clone()
    } else {
        m.root.join(rest)
    };
    Ok((m, inside))
}

/// One located path: the path and its mount id, if the kernel gave one.
pub type Root<'p> = (&'p Path, Option<u64>);

/// Whether `a` and `b` can reach one another through the mount table: located
/// ([`locate`]) on one filesystem (`major:minor`) with one's path inside it
/// equal to or under the other's, or a mount whose mount point is at or under
/// one of them exposing a filesystem path equal to, under or above the other's
/// located path.
pub fn nest(mounts: &[Mount], a: Root<'_>, b: Root<'_>) -> Result<bool, String> {
    let (ma, pa) = locate(mounts, a.0, a.1)?;
    let (mb, pb) = locate(mounts, b.0, b.1)?;
    let meet = |dev: (u32, u32), p: &Path, dev2: (u32, u32), q: &Path| {
        dev == dev2 && (p.starts_with(q) || q.starts_with(p))
    };
    if meet(ma.dev, &pa, mb.dev, &pb) {
        return Ok(true);
    }
    let under = |root: &Path, dev: (u32, u32), p: &Path| {
        mounts.iter().any(|m| m.mount_point.starts_with(root) && meet(m.dev, &m.root, dev, p))
    };
    Ok(under(b.0, ma.dev, &pa) || under(a.0, mb.dev, &pb))
}
