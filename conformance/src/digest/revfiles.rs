//! A run's undo records compared with the ones Core wrote, record for record (CH3a a7-3, D101).
//!
//! - **Layout** (`blockstorage.cpp:509-538`): `magic ‖ u32 LE size ‖ CBlockUndo ‖ SHA256d(parent
//!   hash ‖ CBlockUndo)`. Each block's record goes to the rev file numbered as its blk file
//!   (`:779`), appended as the block connects. A file is zero-padded to its 1 MiB preallocation
//!   chunk (`blockstorage.h:42`) and truncated to its data once finalized (`flatfile.cpp:89`).
//! - **Which record is which.** The run's files hold one record per connected block, heights
//!   `1..=N` in order (V connects each block once, forward): the k-th record is height k's. Its
//!   checksum must be that of the height's parent hash and its undo, and its file the one
//!   numbered as the blk file the run stored the block in ([`run_blocks`]). The run connects each
//!   block as it stores it, so its undo keeps up and a file below its last is finalized (`:685`).
//! - **Core's records** are found by that checksum, whatever file and order Core wrote them in:
//!   its blk files are in download order, so its rev files are not the run's. A found record's
//!   magic, size, undo and checksum are compared byte for byte; the checksum binds the parent hash.
//!   The report counts the equal records Core holds at the run's file and offset too.
//! - **Not found:** the report counts Core's records of a child of the same parent (a checksum of
//!   the parent hash and the record's own undo), and names where the first one, of the run's size
//!   if any, first differs; or it says that Core wrote none. Only the first mismatch is diagnosed,
//!   reading every Core record once. The report names heights, files and offsets, never bytes
//!   (O8 §10.4).
//! - **Divergences and refusals:** a record count other than N, or a run file below its last with
//!   a zero tail, is a divergence of the run. Otherwise the run's files must be records, then
//!   zeros: anything else is refused. A Core file may end in a record a crash cut short, or in
//!   stale bytes past the size Core rewound to: its scan stops there, and the report lists where.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail, ensure};
use dash_types::Numeric as _;
use dashcore::hashes::{Hash, HashEngine, sha256d};

use crate::blkfile;
use crate::check::H256;
use crate::digest::blkdir::BlockDir;

/// The record header: the network magic and the undo's size.
const HEADER: u64 = 8;
/// The checksum after the undo.
const CHECKSUM: u64 = 32;

/// One height of the run: its parent's hash, in its internal byte order, and the number of the
/// blk file the run stored its block in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunBlock {
    pub parent: H256,
    pub file: u32,
}

/// Heights `1..=to` of the run's blocks directory `dir`.
pub fn run_blocks(dir: &BlockDir, to: u32) -> Result<Vec<RunBlock>> {
    ensure!(
        to <= dir.tip(),
        "{}'s best chain ends at {}, below {to}",
        dir.dir().display(),
        dir.tip()
    );
    Ok((1..=to)
        .map(|h| RunBlock {
            parent: dir.hash(h - 1).expect("below the tip").to_lendian(),
            file: dir.location(h).expect("at most the tip").0,
        })
        .collect())
}

/// One undo record: its file, the offset of its undo (after the header, as Core's `nUndoPos`), the
/// undo's size, and the checksum stored after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UndoRecord {
    file: u32,
    pos: u64,
    size: u64,
    checksum: H256,
}

/// A directory's `rev?????.dat` files, scanned.
struct RevFiles {
    files: BTreeMap<u32, File>,
    records: Vec<UndoRecord>,
    /// The files that end in a zero tail, with its offset.
    tails: Vec<(u32, u64)>,
    /// Where a lenient scan stopped short of a file's zero tail, and why.
    ragged: Vec<String>,
}

/// How a file's scan ended.
enum End {
    /// At the end of the file.
    Exact,
    /// At the zero tail from this offset.
    Zeros(u64),
    /// Short of the zero tail, at this offset: why.
    Stopped(u64, &'static str),
}

impl RevFiles {
    /// Scans `dir`'s rev files for records under `magic`. `strict` refuses anything but records
    /// and the zero tail; otherwise a file's scan stops at the first other byte.
    fn scan(dir: &Path, magic: [u8; 4], strict: bool) -> Result<RevFiles> {
        ensure!(
            blkfile::xor_key(dir)?.is_none(),
            "{}: obfuscated files (xor.dat): Dash Core v24 writes none",
            dir.display()
        );
        let mut files = BTreeMap::new();
        let mut records = Vec::new();
        let mut tails = Vec::new();
        let mut ragged = Vec::new();
        for (n, path) in list(dir)? {
            let file = File::open(&path).with_context(|| format!("opening {}", path.display()))?;
            match scan_file(n, &file, magic, &mut records)
                .with_context(|| format!("reading {}", path.display()))?
            {
                End::Exact => {}
                End::Zeros(at) => tails.push((n, at)),
                End::Stopped(at, why) => {
                    let issue = format!("rev{n:05}.dat@{at}: {why}");
                    if strict {
                        bail!("{}: {issue}", dir.display());
                    }
                    ragged.push(issue);
                }
            }
            files.insert(n, file);
        }
        Ok(RevFiles {
            files,
            records,
            tails,
            ragged,
        })
    }

    /// `r`'s bytes: header, undo and checksum.
    fn read(&self, r: &UndoRecord) -> Result<Vec<u8>> {
        let len = usize::try_from(HEADER + r.size + CHECKSUM).context("a record past memory")?;
        let mut out = vec![0; len];
        self.files[&r.file]
            .read_exact_at(&mut out, r.pos - HEADER)
            .with_context(|| format!("reading rev{:05}.dat@{}", r.file, r.pos - HEADER))?;
        Ok(out)
    }
}

/// `rev?????.dat` files (five digits), by number.
fn list(dir: &Path) -> Result<Vec<(u32, PathBuf)>> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).with_context(|| format!("listing {}", dir.display()))? {
        let e = e?;
        let name = e.file_name();
        if let Some(n) = name.to_str().and_then(|s| s.strip_prefix("rev")?.strip_suffix(".dat"))
            && n.len() == 5
            && n.bytes().all(|b| b.is_ascii_digit())
        {
            out.push((n.parse().expect("five digits"), e.path()));
        }
    }
    out.sort();
    ensure!(!out.is_empty(), "no rev*.dat files in {}", dir.display());
    Ok(out)
}

/// Appends file `n`'s records to `out`; how the scan ended.
fn scan_file(n: u32, file: &File, magic: [u8; 4], out: &mut Vec<UndoRecord>) -> Result<End> {
    let len = file.metadata()?.len();
    let mut at = 0;
    while at < len {
        // A zero tail may be shorter than a header: the chunk ends where it ends.
        let mut header = [0; HEADER as usize];
        let got = (len - at).min(HEADER) as usize;
        file.read_exact_at(&mut header[..got], at)?;
        if header[..got].iter().all(|&b| b == 0) {
            return Ok(if zeros(file, at, len)? {
                End::Zeros(at)
            } else {
                End::Stopped(at, "bytes after the zero tail")
            });
        }
        if got < HEADER as usize {
            return Ok(End::Stopped(at, "a header cut short"));
        }
        if header[..4] != magic {
            return Ok(End::Stopped(at, "not the network magic"));
        }
        let size = u64::from(u32::from_le_bytes(header[4..].try_into().expect("4 bytes")));
        let pos = at + HEADER;
        if len - pos < size + CHECKSUM {
            return Ok(End::Stopped(at, "a record that does not fit in the file"));
        }
        let mut checksum = [0; CHECKSUM as usize];
        file.read_exact_at(&mut checksum, pos + size)?;
        out.push(UndoRecord {
            file: n,
            pos,
            size,
            checksum,
        });
        at = pos + size + CHECKSUM;
    }
    Ok(End::Exact)
}

/// A record's undo, between its header and its checksum.
fn undo_of(record: &[u8]) -> &[u8] {
    &record[HEADER as usize..record.len() - CHECKSUM as usize]
}

/// Whether `file`'s bytes `from..len` are all zero.
fn zeros(file: &File, from: u64, len: u64) -> Result<bool> {
    let mut buf = vec![0; 1 << 16];
    let mut at = from;
    while at < len {
        let n = (len - at).min(buf.len() as u64) as usize;
        file.read_exact_at(&mut buf[..n], at)?;
        if buf[..n].iter().any(|&b| b != 0) {
            return Ok(false);
        }
        at += n as u64;
    }
    Ok(true)
}

/// Core's checksum of an undo record: `SHA256d(parent hash ‖ undo)` (`blockstorage.cpp:532-535`).
pub fn checksum(parent: &H256, undo: &[u8]) -> H256 {
    let mut e = sha256d::Hash::engine();
    e.input(parent);
    e.input(undo);
    sha256d::Hash::from_engine(e).to_byte_array()
}

/// The comparison of a run's records with Core's.
#[derive(Debug)]
pub struct RevReport {
    /// Heights compared: `1..=compared`.
    pub compared: u32,
    pub mismatched: u32,
    /// The first mismatch: its height and what differs.
    pub first: Option<(u32, String)>,
    /// Divergences of the run's files: a record count other than the heights', a file below the
    /// last with a zero tail.
    pub run_issues: Vec<String>,
    /// Equal records at the same file and offset in both: where both sides' files hold the same
    /// blocks in the same order, the run's records are a byte prefix of Core's file.
    pub same_position: u32,
    /// The height to which every record is equal and at the same file and offset (0 if height 1's
    /// is not): heights `1..=same_position_to` are a byte prefix of Core's files.
    pub same_position_to: u32,
    /// Core's records, and how many repeat an earlier one's checksum: siblings with equal undo
    /// (coinbase-only children of one parent), or a record written again after a crash lost its
    /// index entry.
    pub core_records: usize,
    pub core_duplicates: usize,
    /// Where Core's files stopped short of their zero tails.
    pub core_ragged: Vec<String>,
}

impl RevReport {
    /// Whether the run's records differ from Core's: a mismatch or an issue.
    pub fn diverged(&self) -> bool {
        self.mismatched > 0 || !self.run_issues.is_empty()
    }
}

/// Compares the undo records in `ours` (a run's blocks directory) with Core's in `core`, heights
/// `1..=blocks.len()`: `blocks[k]` is height k+1's. At least one height is compared.
pub fn compare(ours: &Path, core: &Path, magic: [u8; 4], blocks: &[RunBlock]) -> Result<RevReport> {
    ensure!(
        !blocks.is_empty(),
        "no heights to compare: the run's records are heights 1..=N, N ≥ 1"
    );
    let ours = RevFiles::scan(ours, magic, true)?;
    let core = RevFiles::scan(core, magic, false)?;
    let mut run_issues = Vec::new();
    if ours.records.len() != blocks.len() {
        run_issues.push(format!(
            "the run wrote {} undo records, for heights 1..={}",
            ours.records.len(),
            blocks.len()
        ));
    }
    let last = ours.files.keys().next_back().copied();
    for &(n, at) in ours.tails.iter().filter(|(n, _)| Some(*n) != last) {
        run_issues.push(format!(
            "rev{n:05}.dat@{at}: a zero tail below the last file (finalizing truncates it, \
             flatfile.cpp:89)"
        ));
    }
    let mut by_checksum = HashMap::with_capacity(core.records.len());
    for r in &core.records {
        by_checksum.entry(r.checksum).or_insert(*r);
    }
    let mut report = RevReport {
        compared: ours.records.len().min(blocks.len()) as u32,
        mismatched: 0,
        first: None,
        run_issues,
        same_position: 0,
        same_position_to: 0,
        core_records: core.records.len(),
        core_duplicates: core.records.len() - by_checksum.len(),
        core_ragged: core.ragged.clone(),
    };
    for ((r, b), height) in ours.records.iter().zip(blocks).zip(1..) {
        let mine = ours.read(r)?;
        let mismatch = if checksum(&b.parent, undo_of(&mine)) != r.checksum {
            Some(Mismatch::Parent)
        } else if r.file != b.file {
            Some(Mismatch::File)
        } else if let Some(c) = by_checksum.get(&r.checksum) {
            // Equal checksums leave only a collision to differ; the bytes are compared anyway.
            let theirs = core.read(c)?;
            let equal = theirs == mine;
            if equal && (c.file, c.pos) == (r.file, r.pos) {
                report.same_position += 1;
                // The prefix grows only while every height below this one is in it.
                if report.same_position_to == height - 1 {
                    report.same_position_to = height;
                }
            }
            (!equal).then(|| {
                Mismatch::Bytes(format!(
                    "Core's record with its checksum at {}",
                    difference(&mine, &theirs, c)
                ))
            })
        } else {
            Some(Mismatch::Unmatched)
        };
        let Some(mismatch) = mismatch else {
            continue;
        };
        report.mismatched += 1;
        if report.first.is_none() {
            let what = match mismatch {
                Mismatch::Parent => "the record is not of a child of the height's parent (out of \
                                     order, or a wrong checksum)"
                    .to_owned(),
                Mismatch::File => format!(
                    "the record is in rev{:05}.dat, its block in blk{:05}.dat \
                     (blockstorage.cpp:779)",
                    r.file, b.file
                ),
                Mismatch::Bytes(what) => what,
                Mismatch::Unmatched => diagnose(&core, &b.parent, &mine)?,
            };
            report.first = Some((height, what));
        }
    }
    Ok(report)
}

/// How a run's record differs from Core's.
enum Mismatch {
    /// Its checksum is not that of the height's parent hash and its undo.
    Parent,
    /// It is not in the rev file of its block's blk file.
    File,
    /// Core's record with its checksum differs: where.
    Bytes(String),
    /// No Core record has its checksum.
    Unmatched,
}

/// Where Core's record `c` first differs from the run's `mine`.
fn difference(mine: &[u8], theirs: &[u8], c: &UndoRecord) -> String {
    let at =
        mine.iter().zip(theirs).position(|(a, b)| a != b).unwrap_or(mine.len().min(theirs.len()));
    format!(
        "rev{:05}.dat@{} differs from byte {at} (the run's {} bytes, Core's {})",
        c.file,
        c.pos - HEADER,
        mine.len(),
        theirs.len()
    )
}

/// For a run's record `mine` that no Core record's checksum matches: how many Core records are of
/// a child of `parent`, and where the first, of `mine`'s size if any, differs; or that there are
/// none.
fn diagnose(core: &RevFiles, parent: &H256, mine: &[u8]) -> Result<String> {
    let mut children = Vec::new();
    for c in &core.records {
        let theirs = core.read(c)?;
        if checksum(parent, undo_of(&theirs)) == c.checksum {
            children.push((c, theirs));
        }
    }
    let Some((c, theirs)) =
        children.iter().find(|(_, t)| t.len() == mine.len()).or(children.first())
    else {
        let ragged = if core.ragged.is_empty() {
            String::new()
        } else {
            format!("; {} of Core's files end early", core.ragged.len())
        };
        return Ok(format!("Core wrote no undo record of a child of its parent{ragged}"));
    };
    Ok(format!(
        "Core holds {} record(s) of a child of its parent; the one at {}",
        children.len(),
        difference(mine, theirs, c)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAGIC: [u8; 4] = [0xce, 0xe2, 0xca, 0xff];

    fn parent(h: u8) -> H256 {
        [h; 32]
    }

    fn record(parent: &H256, undo: &[u8]) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        out.extend_from_slice(&(undo.len() as u32).to_le_bytes());
        out.extend_from_slice(undo);
        out.extend_from_slice(&checksum(parent, undo));
        out
    }

    /// Writes `name` in `dir`: `records`, then `pad` zeros.
    fn file(dir: &Path, name: &str, records: &[Vec<u8>], pad: usize) {
        let mut bytes = records.concat();
        bytes.resize(bytes.len() + pad, 0);
        std::fs::write(dir.join(name), bytes).unwrap();
    }

    fn undo(h: u8) -> Vec<u8> {
        vec![h; 10 + h as usize]
    }

    /// The record of parent `h` and undo `h`: 51 bytes for 1, one more for each next `h`.
    fn rec(h: u8) -> Vec<u8> {
        record(&parent(h), &undo(h))
    }

    /// The run: heights 1..=3 in one file. Core: the same three across two files, out of order,
    /// with another block's record and a repeat.
    fn fixture() -> (tempfile::TempDir, tempfile::TempDir) {
        let (ours, core) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        file(ours.path(), "rev00000.dat", &[rec(1), rec(2), rec(3)], 4096);
        file(core.path(), "rev00000.dat", &[rec(2), rec(9)], 100);
        file(core.path(), "rev00001.dat", &[rec(1), rec(3), rec(1)], 0);
        (ours, core)
    }

    /// Heights 1..=3, each a child of parent `h`, stored in blk file `files[h - 1]`.
    fn blocks(files: [u32; 3]) -> Vec<RunBlock> {
        (1..=3)
            .map(|h| RunBlock {
                parent: parent(h),
                file: files[h as usize - 1],
            })
            .collect()
    }

    fn compare_fixture(ours: &Path, core: &Path) -> RevReport {
        compare(ours, core, MAGIC, &blocks([0; 3])).unwrap()
    }

    #[test]
    fn records_found_by_checksum_in_any_file_and_order_compare_equal() {
        let (ours, core) = fixture();
        let r = compare_fixture(ours.path(), core.path());
        assert_eq!((r.compared, r.mismatched, r.first.clone()), (3, 0, None));
        assert_eq!((r.core_records, r.core_duplicates, r.same_position), (5, 1, 0));
        assert_eq!(r.same_position_to, 0);
        assert!(r.core_ragged.is_empty() && r.run_issues.is_empty() && !r.diverged());
    }

    #[test]
    fn records_at_the_run_s_file_and_offset_are_counted() {
        let (ours, core) = fixture();
        file(core.path(), "rev00000.dat", &[rec(1), rec(2), rec(9)], 0);
        let r = compare_fixture(ours.path(), core.path());
        assert_eq!((r.mismatched, r.same_position, r.core_duplicates), (0, 2, 2));
        assert_eq!(r.same_position_to, 2);
        // Height 2 alone at its offset, behind another block's record of height 1's size: counted,
        // but the prefix ends before height 1.
        let other = record(&parent(7), &undo(1));
        file(core.path(), "rev00000.dat", &[other, rec(2), rec(9)], 0);
        let r = compare_fixture(ours.path(), core.path());
        assert_eq!((r.mismatched, r.same_position, r.same_position_to), (0, 1, 0));
    }

    #[test]
    fn a_run_of_no_heights_is_refused() {
        let (ours, core) = fixture();
        let e = compare(ours.path(), core.path(), MAGIC, &[]).unwrap_err().to_string();
        assert_eq!(e, "no heights to compare: the run's records are heights 1..=N, N ≥ 1");
    }

    #[test]
    fn a_core_record_that_differs_is_named_with_its_first_differing_byte() {
        let (ours, core) = fixture();
        let mut changed = undo(2);
        changed[4] ^= 1;
        let longer = [undo(2), vec![0]].concat();
        file(
            core.path(),
            "rev00000.dat",
            &[record(&parent(2), &longer), record(&parent(2), &changed)],
            0,
        );
        let r = compare_fixture(ours.path(), core.path());
        assert_eq!(r.mismatched, 1);
        // The child of the run's size is the one named.
        assert_eq!(
            r.first,
            Some((
                2,
                "Core holds 2 record(s) of a child of its parent; the one at rev00000.dat@53 \
                 differs from byte 12 (the run's 52 bytes, Core's 52)"
                    .to_owned()
            ))
        );
    }

    #[test]
    fn a_height_core_has_no_record_of_is_a_mismatch() {
        let (ours, core) = fixture();
        file(core.path(), "rev00001.dat", &[rec(1)], 0);
        let r = compare_fixture(ours.path(), core.path());
        assert_eq!(r.mismatched, 1);
        assert_eq!(r.first, Some((3, "Core wrote no undo record of a child of its parent".into())));
    }

    #[test]
    fn a_run_record_not_of_its_height_s_parent_is_a_mismatch() {
        let (ours, core) = fixture();
        let mut wrong = blocks([0; 3]);
        wrong[1].parent = parent(7);
        let r = compare(ours.path(), core.path(), MAGIC, &wrong).unwrap();
        assert_eq!(r.mismatched, 1);
        assert_eq!(
            r.first,
            Some((
                2,
                "the record is not of a child of the height's parent (out of order, or a wrong \
                 checksum)"
                    .into()
            ))
        );
    }

    #[test]
    fn a_run_record_outside_its_block_s_file_number_is_a_mismatch() {
        let (ours, core) = fixture();
        let r = compare(ours.path(), core.path(), MAGIC, &blocks([0, 0, 1])).unwrap();
        assert_eq!(r.mismatched, 1);
        assert_eq!(
            r.first,
            Some((
                3,
                "the record is in rev00000.dat, its block in blk00001.dat (blockstorage.cpp:779)"
                    .into()
            ))
        );
    }

    #[test]
    fn a_record_count_other_than_the_heights_is_a_divergence() {
        let (ours, core) = fixture();
        let r = compare(ours.path(), core.path(), MAGIC, &blocks([0; 3])[..2]).unwrap();
        assert_eq!((r.compared, r.mismatched), (2, 0));
        assert_eq!(r.run_issues, ["the run wrote 3 undo records, for heights 1..=2"]);
        assert!(r.diverged());
    }

    #[test]
    fn a_run_file_below_its_last_must_be_finalized() {
        let (ours, core) = fixture();
        file(ours.path(), "rev00000.dat", &[rec(1), rec(2)], 0);
        file(ours.path(), "rev00001.dat", &[rec(3)], 4096);
        let r = compare(ours.path(), core.path(), MAGIC, &blocks([0, 0, 1])).unwrap();
        assert!(!r.diverged(), "{:?} {:?}", r.first, r.run_issues);
        file(ours.path(), "rev00000.dat", &[rec(1), rec(2)], 10);
        let r = compare(ours.path(), core.path(), MAGIC, &blocks([0, 0, 1])).unwrap();
        assert_eq!(r.mismatched, 0);
        assert_eq!(
            r.run_issues,
            ["rev00000.dat@103: a zero tail below the last file (finalizing truncates it, \
              flatfile.cpp:89)"]
        );
    }

    #[test]
    fn a_zero_tail_shorter_than_a_header_ends_a_file() {
        let (ours, core) = fixture();
        file(ours.path(), "rev00000.dat", &[rec(1), rec(2), rec(3)], 5);
        file(core.path(), "rev00000.dat", &[rec(2), rec(9)], 3);
        let r = compare_fixture(ours.path(), core.path());
        assert!(!r.diverged() && r.core_ragged.is_empty(), "{:?} {:?}", r.first, r.core_ragged);
    }

    #[test]
    fn the_run_s_files_must_be_records_then_zeros() {
        let (ours, core) = fixture();
        let mut bytes = rec(1);
        bytes.extend_from_slice(&[0; 8]);
        bytes.push(1);
        std::fs::write(ours.path().join("rev00000.dat"), bytes).unwrap();
        let e = compare(ours.path(), core.path(), MAGIC, &blocks([0; 3])).unwrap_err();
        assert!(e.to_string().ends_with("rev00000.dat@51: bytes after the zero tail"), "{e}");
        let mut bytes = rec(1);
        bytes[0] ^= 1;
        std::fs::write(ours.path().join("rev00000.dat"), bytes).unwrap();
        let e = compare(ours.path(), core.path(), MAGIC, &blocks([0; 3])).unwrap_err();
        assert!(e.to_string().ends_with("rev00000.dat@0: not the network magic"), "{e}");
    }

    #[test]
    fn a_core_file_cut_short_ends_its_scan_and_is_listed() {
        let (ours, core) = fixture();
        let mut cut = rec(5);
        cut.truncate(20);
        file(core.path(), "rev00001.dat", &[rec(1), rec(3), cut], 0);
        let r = compare_fixture(ours.path(), core.path());
        assert_eq!((r.mismatched, r.core_records, r.core_duplicates), (0, 4, 0));
        assert_eq!(r.core_ragged, ["rev00001.dat@104: a record that does not fit in the file"]);
    }

    #[test]
    fn a_core_file_with_bytes_after_its_zero_tail_is_listed() {
        let (ours, core) = fixture();
        file(core.path(), "rev00000.dat", &[rec(2), rec(9), vec![0; 10], vec![1]], 0);
        let r = compare_fixture(ours.path(), core.path());
        assert!(!r.diverged(), "{:?}", r.first);
        assert_eq!(r.core_ragged, ["rev00000.dat@111: bytes after the zero tail"]);
    }

    #[test]
    fn rev_files_are_numbered_with_five_digits() {
        let (ours, core) = fixture();
        file(core.path(), "rev1.dat", &[rec(7)], 0);
        file(core.path(), "rev+0001.dat", &[rec(7)], 0);
        let r = compare_fixture(ours.path(), core.path());
        assert_eq!(r.core_records, 5);
    }

    #[test]
    fn the_checksum_is_cores_double_sha256_of_parent_then_undo() {
        let mut bytes = parent(1).to_vec();
        bytes.extend_from_slice(&undo(1));
        assert_eq!(checksum(&parent(1), &undo(1)), sha256d::Hash::hash(&bytes).to_byte_array());
    }
}
