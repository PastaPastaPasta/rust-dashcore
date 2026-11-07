//! Replaying a `dm-log-writes` log onto an image file, and cheap copies of the result.
//!
//! [`Replayer`] applies entries in log order to one image, so walking every replay point costs one
//! pass over the log. [`Replayer::snapshot`] reflinks the image (`FICLONE`; /work is XFS with
//! reflink), so each point gets its own writable copy without copying data. Where the filesystem
//! can't reflink (a CI runner's ext4 temporary directory, BL-609), [`reflink`] copies the image's
//! data instead.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::os::fd::AsRawFd;
use std::path::Path;

use super::logwrites::{DISCARD, Entry, Log, Point};

/// `FICLONE`: `_IOW(0x94, 9, int)`.
const FICLONE: libc::c_ulong = 0x4004_9409;

/// Makes `dst` (created, or truncated) a reflink copy of `src`; where the filesystem can't reflink
/// (ext4, tmpfs) or `dst` is on another one, a `sparse_copy` (BL-609).
pub fn reflink(src: &File, dst: &Path) -> io::Result<File> {
    let out = OpenOptions::new().read(true).write(true).create(true).truncate(true).open(dst)?;
    // SAFETY: both descriptors are open files; FICLONE reads only the source descriptor argument.
    let rc = unsafe { libc::ioctl(out.as_raw_fd(), FICLONE, src.as_raw_fd()) };
    if rc != 0 {
        let e = io::Error::last_os_error();
        match e.raw_os_error() {
            Some(libc::EOPNOTSUPP | libc::EXDEV | libc::ENOTTY) => {
                // Said once: a run root off XFS copies every image, slowly.
                static SAID: std::sync::Once = std::sync::Once::new();
                SAID.call_once(|| {
                    eprintln!("st3b: no reflink at {} ({e}): copying", dst.display())
                });
                sparse_copy(src, &out)?;
            }
            _ => return Err(e),
        }
    }
    Ok(out)
}

/// Copies `src`'s data regions (`SEEK_DATA` to `SEEK_HOLE`) into `out` at their offsets, `out`
/// emptied and given `src`'s length: `src`'s holes stay holes and read back as zeros, as they do
/// in `src`. Positional reads and writes only (the seeks move `src`'s offset, which no caller
/// reads by).
fn sparse_copy(src: &File, out: &File) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    let len = src.metadata()?.len();
    // Emptied first: whatever `out` held reads back as `src`'s holes, zeros.
    out.set_len(0)?;
    out.set_len(len)?;
    let mut buf = vec![0u8; 1 << 20];
    let mut at = 0;
    while let Some(data) = seek(src, at, libc::SEEK_DATA)? {
        let hole = seek(src, data, libc::SEEK_HOLE)?.unwrap_or(len);
        let mut pos = data;
        while pos < hole {
            let n = (hole - pos).min(buf.len() as u64) as usize;
            src.read_exact_at(&mut buf[..n], pos)?;
            out.write_all_at(&buf[..n], pos)?;
            pos += n as u64;
        }
        at = hole;
    }
    Ok(())
}

/// `lseek(f, from, whence)`; `None` at ENXIO (no data from `from` on).
fn seek(f: &File, from: u64, whence: libc::c_int) -> io::Result<Option<u64>> {
    // SAFETY: an open descriptor; lseek only moves its offset.
    let at = unsafe { libc::lseek(f.as_raw_fd(), from as libc::off_t, whence) };
    if at >= 0 {
        return Ok(Some(at as u64));
    }
    let e = io::Error::last_os_error();
    if e.raw_os_error() == Some(libc::ENXIO) {
        Ok(None)
    } else {
        Err(e)
    }
}

/// Writes `entry` (its data read from `log`) into `image`; a discard punches a hole, which reads
/// back as zeros. Flushes and marks write nothing.
pub fn apply(log: &mut File, image: &File, entry: &Entry, block: u64) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    let offset = entry.sector * block;
    let len = entry.nr_sectors * block;
    if entry.is(DISCARD) {
        if len == 0 {
            return Ok(());
        }
        let (off, n) = (offset as libc::off_t, len as libc::off_t);
        let mode = libc::FALLOC_FL_PUNCH_HOLE | libc::FALLOC_FL_KEEP_SIZE;
        // SAFETY: an open descriptor and a byte range.
        if unsafe { libc::fallocate(image.as_raw_fd(), mode, off, n) } != 0 {
            return Err(io::Error::last_os_error());
        }
        return Ok(());
    }
    if !entry.has_data() {
        return Ok(());
    }
    log.seek(SeekFrom::Start(entry.data_offset))?;
    let mut buf = vec![0u8; len.min(1 << 20) as usize];
    let mut done = 0u64;
    while done < len {
        let n = (len - done).min(buf.len() as u64) as usize;
        log.read_exact(&mut buf[..n])?;
        image.write_all_at(&buf[..n], offset + done)?;
        done += n as u64;
    }
    Ok(())
}

/// Writes the listed blocks of data entry `entry` (offsets from its first block, ascending) into
/// `image`: the part of a write that a split point keeps.
pub fn apply_part(
    log: &mut File,
    image: &File,
    entry: &Entry,
    block: u64,
    blocks: &[u64],
) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    assert!(entry.has_data(), "a part of a write");
    let mut buf = vec![0u8; block as usize];
    for &b in blocks {
        assert!(b < entry.nr_sectors, "block {b} of a {}-block write", entry.nr_sectors);
        log.seek(SeekFrom::Start(entry.data_offset + b * block))?;
        log.read_exact(&mut buf)?;
        image.write_all_at(&buf, (entry.sector + b) * block)?;
    }
    Ok(())
}

/// One image advanced through a log.
pub struct Replayer<'a> {
    log: &'a Log,
    data: File,
    image: File,
    applied: usize,
}

impl<'a> Replayer<'a> {
    /// Replays onto `image`, which must hold the device as it was when logging started (zeros, or a
    /// copy of the data device taken before the log was attached); `data` is the log file.
    pub fn new(log: &'a Log, data: File, image: File) -> Self {
        Replayer {
            log,
            data,
            image,
            applied: 0,
        }
    }

    /// How many entries the image holds.
    pub fn applied(&self) -> usize {
        self.applied
    }

    /// Applies entries up to (not including) `prefix`. Prefixes only move forward.
    pub fn advance_to(&mut self, prefix: usize) -> io::Result<()> {
        assert!(prefix >= self.applied, "replay goes forward: {} → {prefix}", self.applied);
        assert!(prefix <= self.log.entries.len(), "prefix {prefix} past the log");
        let block = u64::from(self.log.sector_size);
        for entry in &self.log.entries[self.applied..prefix] {
            apply(&mut self.data, &self.image, entry, block)?;
        }
        self.applied = prefix;
        Ok(())
    }

    /// Writes `point`'s device state to `dst`: the image advanced to its prefix, reflinked, then
    /// its extra entries and its part of one more applied to the copy only, in log order.
    pub fn snapshot(&mut self, point: &Point, dst: &Path) -> io::Result<File> {
        self.advance_to(point.prefix)?;
        self.image.sync_data()?;
        let copy = reflink(&self.image, dst)?;
        let block = u64::from(self.log.sector_size);
        let part = point.part.as_ref();
        let mut order: Vec<usize> =
            point.extra.iter().copied().chain(part.map(|p| p.entry)).collect();
        order.sort_unstable();
        for i in order {
            let entry = &self.log.entries[i];
            match part.filter(|p| p.entry == i) {
                Some(p) => apply_part(&mut self.data, &copy, entry, block, &p.blocks)?,
                None => apply(&mut self.data, &copy, entry, block)?,
            }
        }
        copy.sync_data()?;
        Ok(copy)
    }

    /// The whole log replayed, compared with `device`, the data device as the kernel left it: the
    /// first offset where they differ, or none. A reader that misplaced or dropped an entry would
    /// make every point a state the device never had.
    pub fn whole_log_differs(&mut self, device: &Path) -> io::Result<Option<u64>> {
        self.advance_to(self.log.entries.len())?;
        first_difference(&self.image, &File::open(device)?)
    }
}

/// The first offset at which `a` and `b` differ (a shorter file differs at its end), or none.
pub fn first_difference(a: &File, b: &File) -> io::Result<Option<u64>> {
    use std::os::unix::fs::FileExt;
    let (len_a, len_b) = (a.metadata()?.len(), b.metadata()?.len());
    let len = len_a.min(len_b);
    let (mut x, mut y) = (vec![0u8; 1 << 20], vec![0u8; 1 << 20]);
    let mut at = 0u64;
    while at < len {
        let n = (len - at).min(x.len() as u64) as usize;
        a.read_exact_at(&mut x[..n], at)?;
        b.read_exact_at(&mut y[..n], at)?;
        if let Some(i) = x[..n].iter().zip(&y[..n]).position(|(p, q)| p != q) {
            return Ok(Some(at + i as u64));
        }
        at += n as u64;
    }
    Ok((len_a != len_b).then_some(len))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::FileExt;

    /// A file in `dir` of `len` bytes, with `data` written at each offset.
    fn sparse(dir: &Path, name: &str, len: u64, data: &[(u64, &[u8])]) -> File {
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(dir.join(name))
            .expect("created");
        f.set_len(len).unwrap();
        for (at, bytes) in data {
            f.write_all_at(bytes, *at).unwrap();
        }
        f
    }

    fn bytes(f: &File) -> Vec<u8> {
        let mut out = vec![0u8; f.metadata().unwrap().len() as usize];
        f.read_exact_at(&mut out, 0).unwrap();
        out
    }

    #[test]
    fn a_sparse_copy_has_the_same_bytes_and_length() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let len = (4 << 20) + 5;
        let cases: [&[(u64, &[u8])]; 3] = [
            &[(0, b"head"), (3 << 20, &[7u8; 70_000]), (len - 3, b"end")],
            &[(1 << 20, b"x")],
            &[],
        ];
        for (i, data) in cases.into_iter().enumerate() {
            let src = sparse(dir.path(), &format!("src{i}"), len, data);
            let out = sparse(dir.path(), &format!("out{i}"), 9, &[(0, b"stale")]);
            sparse_copy(&src, &out).unwrap();
            assert_eq!(bytes(&out), bytes(&src), "case {i}");
        }
    }

    /// tmpfs can't reflink: the copy falls back, holes kept (BL-609: a CI runner's ext4 temporary
    /// directory). Skipped where `/dev/shm` is missing or can reflink.
    #[test]
    fn a_copy_where_the_filesystem_cant_reflink_is_a_sparse_copy() {
        use std::os::unix::fs::MetadataExt;
        let Ok(dir) = tempfile::tempdir_in("/dev/shm") else {
            return;
        };
        let len = 4 << 20;
        let src = sparse(dir.path(), "src", len, &[(4096, b"block 1"), (len - 4, b"tail")]);
        let probe = File::create(dir.path().join("probe")).unwrap();
        // SAFETY: both descriptors are open files.
        if unsafe { libc::ioctl(probe.as_raw_fd(), FICLONE, src.as_raw_fd()) } == 0 {
            return;
        }
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EOPNOTSUPP));
        let copy = reflink(&src, &dir.path().join("copy")).expect("a copy");
        assert_eq!(bytes(&copy), bytes(&src));
        assert!(copy.metadata().unwrap().blocks() * 512 < len / 2, "the holes stay holes");
    }

    #[test]
    fn first_difference_finds_the_first_changed_byte_or_a_length_change() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let file = |name: &str, len: u64| {
            let f = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(dir.path().join(name))
                .expect("a file");
            f.set_len(len).expect("a length");
            f
        };
        let (a, b) = (file("a", 3 << 20), file("b", 3 << 20));
        assert_eq!(first_difference(&a, &b).unwrap(), None);
        b.write_all_at(&[1], (2 << 20) + 5).unwrap();
        b.write_all_at(&[1], (2 << 20) + 9).unwrap();
        assert_eq!(first_difference(&a, &b).unwrap(), Some((2 << 20) + 5));
        a.write_all_at(&[1, 0, 0, 0, 1], (2 << 20) + 5).unwrap();
        assert_eq!(first_difference(&a, &b).unwrap(), None);
        a.set_len(4 << 20).unwrap();
        assert_eq!(first_difference(&a, &b).unwrap(), Some(3 << 20));
    }
}
