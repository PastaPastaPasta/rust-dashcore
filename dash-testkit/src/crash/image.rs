//! Replaying a `dm-log-writes` log onto an image file, and cheap copies of the result.
//!
//! [`Replayer`] applies entries in log order to one image, so walking every replay point costs one
//! pass over the log. [`Replayer::snapshot`] reflinks the image (`FICLONE`; /work is XFS with
//! reflink), so each point gets its own writable copy without copying data.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::os::fd::AsRawFd;
use std::path::Path;

use super::logwrites::{DISCARD, Entry, Log, Point};

/// `FICLONE`: `_IOW(0x94, 9, int)`.
const FICLONE: libc::c_ulong = 0x4004_9409;

/// Makes `dst` (created, or truncated) a reflink copy of `src`.
pub fn reflink(src: &File, dst: &Path) -> io::Result<File> {
    let out = OpenOptions::new().read(true).write(true).create(true).truncate(true).open(dst)?;
    // SAFETY: both descriptors are open files; FICLONE reads only the source descriptor argument.
    let rc = unsafe { libc::ioctl(out.as_raw_fd(), FICLONE, src.as_raw_fd()) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(out)
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
