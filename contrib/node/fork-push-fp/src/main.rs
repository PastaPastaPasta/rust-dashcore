//! check-fork-push.sh's check 2: does any pushed blob share a run of at least `--window` bytes
//! (default 64) with a file under a private directory, other than runs that are already public
//! or were reviewed?
//!
//! Method. Let C = window / 2 and S = C / 2. The pushed blobs are spooled to one file on disk
//! (streamed from git, then read back into memory once: under 4 GiB by construction), and
//! indexed by the hash of the C bytes at every blob offset that is a multiple of S (chunks with
//! fewer than `--min-distinct` distinct byte values are not indexed). Each private file is then
//! streamed through a rolling hash of every C-byte window; a window whose hash is in the index is
//! compared byte by byte with the pushed bytes and extended both ways (up to 1 MiB each way). A
//! shared run of `window` bytes or more holds at least two pushed chunks at multiples of S, so it
//! is found when one of them has `--min-distinct` distinct values and its hash occurs at most
//! 4096 times in the index (more equal chunks than that are a repeated pattern; the count of such
//! lookups is printed). Runs are tracked per diagonal (pushed offset minus private offset), so a
//! run on one diagonal never hides another. A run counts unless:
//! - it holds fewer than `--min-distinct` distinct byte values;
//! - the whole run occurs in a blob of `--public` (content already published elsewhere);
//! - every private chunk at a multiple of C that the run covers is in `--pins` (chunks a reviewer
//!   has looked at; pins are content hashes, so a pin exempts that chunk in every pushed blob).
//!
//! A private file shorter than `window` (and at least `--small-min` bytes) counts when it equals a
//! pushed blob. A private file whose git blob id is listed in `--public-ids` (a blob of a public
//! repository's history) is not scanned. Private paths matching a pattern of `--exclude` are not
//! read; the count of files each pattern drops is printed by pattern line number.
//!
//! Usage:
//!   fork-push-fp --repo <git dir> --blobs <file of "<blob sha or file:<path>>\t<path>" lines>
//!                --private <dir> --out <report file> --spool <dir>
//!                [--public <file of "<git dir>\t<blob sha>" lines>] [--public-ids <file>]
//!                [--exclude <patterns file>] [--pins <chunk hash file>] [--write-pins <file>]
//!                [--window 64] [--min-distinct 4] [--small-min 16] [--threads 8]
//!
//! `--exclude` patterns are paths relative to `--private`, one per line (`#` comments): `*` and
//! `?` stay within one path segment, `**` crosses segments, and a trailing `/` matches the
//! directory and everything below it. `--write-pins` writes the chunk hashes of every counted run.
//!
//! Exit status: 0 no counted run, 1 at least one (the report lists them, one
//! "<pushed path>\t<private path relative to --private>\t<longest run>\t<runs>" line each),
//! 2 on any error (an unreadable private file is an error; a file that vanishes during the walk
//! is counted and skipped, since the registry holds live fuzz output). Error lines name private
//! paths, so the caller only shows their count.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

const BASE: u64 = 0x0000_0100_0000_01b3;
/// Extension stops here; the report only needs to show that a run is long.
const EXTEND_CAP: usize = 1 << 20;
/// Index entries examined per chunk; more equal chunks than this are a repeated pattern.
const CANDIDATE_CAP: usize = 4096;
/// Private files are read in segments of this size, each overlapping the last by `OVERLAP`.
const SEGMENT: usize = 64 << 20;
const OVERLAP: usize = 2 * EXTEND_CAP + 4096;

struct Opts {
    repo: PathBuf,
    blobs: PathBuf,
    private: PathBuf,
    out: PathBuf,
    spool: PathBuf,
    public: Option<PathBuf>,
    public_ids: Option<PathBuf>,
    exclude: Option<PathBuf>,
    pins: Option<PathBuf>,
    write_pins: Option<PathBuf>,
    window: usize,
    min_distinct: usize,
    small_min: usize,
    threads: usize,
}

fn usage() -> ! {
    eprintln!(
        "usage: fork-push-fp --repo <git dir> --blobs <list> --private <dir> --out <report> \
         --spool <dir> [--public <list>] [--public-ids <file>] [--exclude <file>] [--pins <file>] \
         [--write-pins <file>] [--window N] [--min-distinct N] [--small-min N] [--threads N]"
    );
    std::process::exit(2);
}

fn parse_args() -> Opts {
    let mut args = std::env::args().skip(1);
    let mut opts = Opts {
        repo: PathBuf::new(),
        blobs: PathBuf::new(),
        private: PathBuf::new(),
        out: PathBuf::new(),
        spool: PathBuf::new(),
        public: None,
        public_ids: None,
        exclude: None,
        pins: None,
        write_pins: None,
        window: 64,
        min_distinct: 4,
        small_min: 16,
        threads: 8,
    };
    let mut required = 0;
    while let Some(flag) = args.next() {
        let value = args.next().unwrap_or_else(|| usage());
        let number = || value.parse::<usize>().unwrap_or_else(|_| usage());
        match flag.as_str() {
            "--repo" => (opts.repo, required) = (PathBuf::from(&value), required | 1),
            "--blobs" => (opts.blobs, required) = (PathBuf::from(&value), required | 2),
            "--private" => (opts.private, required) = (PathBuf::from(&value), required | 4),
            "--out" => (opts.out, required) = (PathBuf::from(&value), required | 8),
            "--spool" => (opts.spool, required) = (PathBuf::from(&value), required | 16),
            "--public" => opts.public = Some(PathBuf::from(&value)),
            "--public-ids" => opts.public_ids = Some(PathBuf::from(&value)),
            "--exclude" => opts.exclude = Some(PathBuf::from(&value)),
            "--pins" => opts.pins = Some(PathBuf::from(&value)),
            "--write-pins" => opts.write_pins = Some(PathBuf::from(&value)),
            "--window" => opts.window = number(),
            "--min-distinct" => opts.min_distinct = number(),
            "--small-min" => opts.small_min = number(),
            "--threads" => opts.threads = number().max(1),
            _ => usage(),
        }
    }
    if required != 31 || opts.window < 8 || !opts.window.is_multiple_of(4) {
        usage();
    }
    opts
}

fn distinct(bytes: &[u8]) -> usize {
    let mut seen = [false; 256];
    bytes.iter().for_each(|&b| seen[b as usize] = true);
    seen.iter().filter(|&&s| s).count()
}

fn chunk_hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0u64, |h, &b| h.wrapping_mul(BASE).wrapping_add(u64::from(b) + 1))
}

/// Rolling hashes of every `c`-byte window of `bytes`, in order.
fn rolling(bytes: &[u8], c: usize) -> impl Iterator<Item = (usize, u64)> + '_ {
    let pow = (1..c).fold(1u64, |p, _| p.wrapping_mul(BASE));
    let mut h = if bytes.len() >= c {
        chunk_hash(&bytes[..c])
    } else {
        0
    };
    (0..(bytes.len() + 1).saturating_sub(c)).map(move |i| {
        if i > 0 {
            let out = u64::from(bytes[i - 1]) + 1;
            let inb = u64::from(bytes[i + c - 1]) + 1;
            h = h.wrapping_sub(out.wrapping_mul(pow)).wrapping_mul(BASE).wrapping_add(inb);
        }
        (i, h)
    })
}

fn mix(h: u64) -> u64 {
    (h ^ (h >> 31)).wrapping_mul(0xbf58_476d_1ce4_e5b9)
}

/// Chunk hashes at some offsets of a spool, as a blocked Bloom filter (three bits in one 512-bit
/// block per hash) in front of a sorted list of (high 32 hash bits, spool offset) keys.
struct Index {
    keys: Vec<u64>,
    bloom: Vec<u64>,
    block_bits: u32,
}

impl Index {
    fn new(mut entries: Vec<(u64, u32)>) -> Index {
        let block_bits = (usize::BITS - (entries.len() / 32).max(1).leading_zeros()).clamp(4, 30);
        let mut bloom = vec![0u64; 8 << block_bits];
        for &(h, _) in &entries {
            for (word, bit) in Self::bits(h, block_bits) {
                bloom[word] |= 1 << bit;
            }
        }
        entries.sort_unstable_by_key(|&(h, off)| (h >> 32, off));
        let keys = entries.into_iter().map(|(h, off)| (h >> 32) << 32 | u64::from(off)).collect();
        Index {
            keys,
            bloom,
            block_bits,
        }
    }

    fn bits(h: u64, block_bits: u32) -> [(usize, u32); 3] {
        let m = mix(h);
        let block = (m >> (64 - block_bits)) as usize * 8;
        let b = |shift: u32| {
            let v = (m >> shift) & 511;
            (block + (v / 64) as usize, (v % 64) as u32)
        };
        [b(0), b(9), b(18)]
    }

    /// Spool offsets whose chunk hash has the high 32 bits of `h` (a superset of the equal chunks).
    fn candidates(&self, h: u64) -> &[u64] {
        if Self::bits(h, self.block_bits).iter().any(|&(w, b)| self.bloom[w] >> b & 1 == 0) {
            return &[];
        }
        let lo = self.keys.partition_point(|&k| k >> 32 < h >> 32);
        let hi = lo + self.keys[lo..].partition_point(|&k| k >> 32 == h >> 32);
        &self.keys[lo..hi]
    }
}

/// Blobs concatenated into one file, read back with positioned reads.
struct Spool {
    data: Vec<u8>,
    /// Start offset of each blob, ascending, plus a final end offset.
    starts: Vec<u64>,
    paths: Vec<Vec<String>>,
}

impl Spool {
    fn blob_at(&self, offset: u64) -> usize {
        self.starts.partition_point(|&s| s <= offset) - 1
    }

    fn bounds(&self, blob: usize) -> (u64, u64) {
        (self.starts[blob], self.starts[blob + 1])
    }

    fn len(&self) -> u64 {
        *self.starts.last().unwrap_or(&0)
    }

    fn bytes(&self, offset: u64, len: usize) -> &[u8] {
        &self.data[offset as usize..offset as usize + len]
    }

    fn blob(&self, blob: usize) -> &[u8] {
        let (lo, hi) = self.bounds(blob);
        self.bytes(lo, (hi - lo) as usize)
    }

    /// Indexes the `c`-byte chunk at every blob offset that is a multiple of `step`, skipping
    /// chunks with fewer than `min_distinct` distinct bytes.
    fn index(&self, c: usize, step: usize, min_distinct: usize) -> Index {
        let mut entries = Vec::new();
        for blob in 0..self.paths.len() {
            let bytes = self.blob(blob);
            let lo = self.starts[blob];
            let mut at = 0;
            while at + c <= bytes.len() {
                let chunk = &bytes[at..at + c];
                if distinct(chunk) >= min_distinct.min(c) {
                    entries.push((chunk_hash(chunk), (lo + at as u64) as u32));
                }
                at += step;
            }
        }
        Index::new(entries)
    }

    /// Whether `run` occurs whole in one blob. `run` is at least 2c - 1 bytes long, so an
    /// occurrence contains a chunk at a multiple of `c`, which `index` (built with step `c` and no
    /// distinct-byte floor) holds.
    fn contains(&self, index: &Index, run: &[u8], c: usize) -> bool {
        // An occurrence whose aligned chunks sit at run offsets k, k + c, ... (k < c) holds every
        // one of them in `index`, so only the rarest of them needs looking up.
        let hashes: Vec<u64> = rolling(run, c).map(|(_, h)| h).collect();
        for k in 0..c.min(hashes.len()) {
            let Some((j, candidates)) = (k..hashes.len())
                .step_by(c)
                .map(|j| (j, index.candidates(hashes[j])))
                .min_by_key(|(_, candidates)| candidates.len())
            else {
                continue;
            };
            for &key in candidates.iter().take(CANDIDATE_CAP) {
                let p = key & 0xffff_ffff;
                let (lo, hi) = self.bounds(self.blob_at(p));
                if p - lo < j as u64 || p - j as u64 + run.len() as u64 > hi {
                    continue;
                }
                if self.bytes(p - j as u64, run.len()) == run {
                    return true;
                }
            }
        }
        false
    }
}

/// Spools blobs named by "<git dir>\t<sha>" lines (`repo` = None) or by "<sha>\t<path>" lines of
/// `repo`, where "<sha>" may instead be "file:<path>" for a file's content (the pushed side's
/// decompressed members).
fn spool(list: &Path, repo: Option<&Path>, dest: &Path) -> io::Result<Spool> {
    // Per source (a git dir, or "file:" for files), the blobs in first-seen order.
    let mut sources: Vec<(PathBuf, Vec<String>)> = Vec::new();
    let mut seen: HashMap<(usize, String), usize> = HashMap::new();
    let mut named: Vec<Vec<String>> = Vec::new();
    for line in BufReader::new(fs::File::open(list)?).lines() {
        let line = line?;
        let Some((a, b)) = line.split_once('\t') else {
            return Err(io::Error::other(format!("{}: malformed line: {line}", list.display())));
        };
        let (dir, sha, path) = match repo {
            Some(_) if a.starts_with("file:") => (PathBuf::from("file:"), a, b.to_string()),
            Some(repo) => (repo.to_path_buf(), a, b.to_string()),
            None => (PathBuf::from(a), b, String::new()),
        };
        let s = match sources.iter().position(|(d, _)| *d == dir) {
            Some(s) => s,
            None => {
                sources.push((dir, Vec::new()));
                sources.len() - 1
            }
        };
        let index = *seen.entry((s, sha.to_string())).or_insert_with(|| {
            sources[s].1.push(sha.to_string());
            named.push(Vec::new());
            named.len() - 1
        });
        if !path.is_empty() {
            named[index].push(path);
        }
    }
    let mut out = BufWriter::with_capacity(1 << 20, fs::File::create(dest)?);
    let mut starts = Vec::with_capacity(named.len() + 1);
    let mut paths = Vec::with_capacity(named.len());
    let mut at = 0u64;
    for (s, (dir, shas)) in sources.iter().enumerate() {
        for sha in shas {
            paths.push(std::mem::take(&mut named[seen[&(s, sha.clone())]]));
        }
        if dir.as_os_str() == "file:" {
            for sha in shas {
                starts.push(at);
                at += io::copy(&mut fs::File::open(&sha["file:".len()..])?, &mut out)?;
            }
            continue;
        }
        let mut child = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let mut stdin = child.stdin.take().ok_or_else(|| io::Error::other("no stdin"))?;
        let feed = shas.clone();
        let feeder = std::thread::spawn(move || -> io::Result<()> {
            let mut w = BufWriter::new(&mut stdin);
            for sha in &feed {
                writeln!(w, "{sha}")?;
            }
            w.flush()
        });
        let mut reader =
            BufReader::new(child.stdout.take().ok_or_else(|| io::Error::other("no stdout"))?);
        for sha in shas {
            let mut header = String::new();
            reader.read_line(&mut header)?;
            let mut parts = header.split_whitespace();
            let (got, kind, size) = (parts.next(), parts.next(), parts.next());
            if got != Some(sha.as_str()) || kind != Some("blob") {
                return Err(io::Error::other(format!(
                    "{}: git cat-file: not a blob: {}",
                    dir.display(),
                    header.trim()
                )));
            }
            let size: u64 =
                size.and_then(|s| s.parse().ok()).ok_or_else(|| io::Error::other("bad size"))?;
            starts.push(at);
            if io::copy(&mut (&mut reader).take(size), &mut out)? != size {
                return Err(io::Error::other("git cat-file: short blob"));
            }
            at += size;
            let mut newline = [0u8; 1];
            reader.read_exact(&mut newline)?;
        }
        drop(reader);
        feeder.join().map_err(|_| io::Error::other("feeder panicked"))??;
        if !child.wait()?.success() {
            return Err(io::Error::other(format!("{}: git cat-file failed", dir.display())));
        }
    }
    starts.push(at);
    out.flush()?;
    drop(out);
    if at >= u64::from(u32::MAX) {
        return Err(io::Error::other(format!("{}: blobs exceed 4 GiB", list.display())));
    }
    Ok(Spool {
        data: fs::read(dest)?,
        starts,
        paths,
    })
}

/// `*`/`?` within a segment, `**` across segments; a pattern ending in `/` is a directory prefix.
fn glob(pattern: &[u8], path: &[u8]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((b'*', rest)) if rest.first() == Some(&b'*') => {
            let rest = rest.strip_prefix(b"*/").unwrap_or(&rest[1..]);
            (0..=path.len()).any(|i| glob(rest, &path[i..]))
        }
        Some((b'*', rest)) => {
            let end = path.iter().position(|&b| b == b'/').unwrap_or(path.len());
            (0..=end).any(|i| glob(rest, &path[i..]))
        }
        Some((b'?', rest)) => path.first().is_some_and(|&b| b != b'/') && glob(rest, &path[1..]),
        Some((&p, rest)) => path.first() == Some(&p) && glob(rest, &path[1..]),
    }
}

/// The first pattern that excludes `rel` (a directory when `dir`).
fn excluded(patterns: &[(usize, String)], rel: &str, dir: bool) -> Option<usize> {
    patterns.iter().position(|(_, p)| match p.strip_suffix('/') {
        Some(d) => {
            (dir && glob(d.as_bytes(), rel.as_bytes()))
                || rel.match_indices('/').any(|(i, _)| glob(d.as_bytes(), &rel.as_bytes()[..i]))
        }
        None => glob(p.as_bytes(), rel.as_bytes()),
    })
}

struct Walk<'a> {
    root: &'a Path,
    exclude: &'a [(usize, String)],
    files: Vec<PathBuf>,
    /// Files dropped per pattern of `exclude`.
    dropped: Vec<usize>,
    symlinks: usize,
    vanished: usize,
    errors: Vec<String>,
}

impl Walk<'_> {
    fn dir(&mut self, dir: &Path, drop: Option<usize>) {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.vanished += 1;
                return;
            }
            Err(e) => {
                self.errors.push(format!("{}: {e}", dir.display()));
                return;
            }
        };
        for entry in entries {
            let path = match entry {
                Ok(entry) => entry.path(),
                Err(e) => {
                    self.errors.push(format!("{}: {e}", dir.display()));
                    continue;
                }
            };
            let rel = path.strip_prefix(self.root).unwrap_or(&path).to_string_lossy().into_owned();
            match fs::symlink_metadata(&path) {
                Ok(m) if m.is_dir() => {
                    let drop = drop.or_else(|| excluded(self.exclude, &rel, true));
                    self.dir(&path, drop);
                }
                Ok(m) if m.is_file() => {
                    match drop.or_else(|| excluded(self.exclude, &rel, false)) {
                        Some(p) => self.dropped[p] += 1,
                        None => self.files.push(path),
                    }
                }
                Ok(m) if m.file_type().is_symlink() => self.symlinks += 1,
                Ok(_) => {} // sockets, fifos: no content of their own
                Err(e) if e.kind() == io::ErrorKind::NotFound => self.vanished += 1,
                Err(e) => self.errors.push(format!("{}: {e}", path.display())),
            }
        }
    }
}

/// SHA-1, for git blob ids ("blob <len>\0" + content).
fn git_blob_id(bytes: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476, 0xc3d2_e1f0];
    let mut msg = format!("blob {}\0", bytes.len()).into_bytes();
    msg.extend_from_slice(bytes);
    let bit_len = (msg.len() as u64) * 8;
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for block in msg.as_chunks::<64>().0 {
        let mut w = [0u32; 80];
        for (i, word) in block.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes(*word);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5a82_7999),
                20..=39 => (b ^ c ^ d, 0x6ed9_eba1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1b_bcdc),
                _ => (b ^ c ^ d, 0xca62_c1d6),
            };
            let t =
                a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(wi);
            (e, d, c, b, a) = (d, c, b.rotate_left(30), a, t);
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 20];
    for (i, x) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&x.to_be_bytes());
    }
    out
}

fn parse_hex<const N: usize>(s: &str) -> Option<[u8; N]> {
    let mut out = [0u8; N];
    if s.len() != 2 * N {
        return None;
    }
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(out)
}

/// Per private file: pushed blob -> (runs counted, longest run, private chunk hashes covered).
type FileHits = HashMap<usize, (usize, usize, Vec<u64>)>;

/// Per private file, while it is scanned.
#[derive(Default)]
struct Runs {
    /// Per diagonal (pushed offset - private offset), the private offset its last run reached.
    covered: HashMap<i64, usize>,
    /// Per pushed blob, the private spans of runs already evaluated (the latest few). A run inside
    /// one of them cannot change the result: a part of an exempt run is exempt (it is public too,
    /// or its chunks are pinned, or it has fewer distinct bytes), and a counted run's blob is
    /// already reported. Repetitive data puts runs on thousands of diagonals.
    spans: HashMap<usize, Vec<(usize, usize)>>,
}

/// Evaluated spans kept per pushed blob.
const SPANS_KEPT: usize = 16;

#[derive(Default)]
struct Counters {
    capped: AtomicUsize,
    public_files: AtomicUsize,
    scanned_bytes: AtomicUsize,
}

struct Scan<'a> {
    window: usize,
    min_distinct: usize,
    small_min: usize,
    pushed: &'a Spool,
    index: &'a Index,
    /// Small pushed blobs (shorter than the window) by content.
    small: HashMap<Vec<u8>, usize>,
    public: Option<(&'a Spool, &'a Index)>,
    public_ids: HashSet<[u8; 20]>,
    pins: HashSet<u64>,
    counters: Counters,
}

impl Scan<'_> {
    /// Scans one private file, whole or in overlapping segments.
    fn file(&self, path: &Path) -> io::Result<FileHits> {
        let mut file = fs::File::open(path)?;
        let len = file.metadata()?.len() as usize;
        self.counters.scanned_bytes.fetch_add(len, Ordering::Relaxed);
        let mut hits = FileHits::new();
        if len <= SEGMENT {
            let mut bytes = Vec::with_capacity(len);
            file.read_to_end(&mut bytes)?;
            if !self.public_ids.is_empty() && self.public_ids.contains(&git_blob_id(&bytes)) {
                self.counters.public_files.fetch_add(1, Ordering::Relaxed);
                return Ok(hits);
            }
            if bytes.len() < self.window {
                self.small_file(&bytes, &mut hits);
            } else {
                self.segment(&bytes, 0, 0, &mut Runs::default(), &mut hits)?;
            }
            return Ok(hits);
        }
        let mut runs = Runs::default();
        let mut buf = vec![0u8; SEGMENT];
        let (mut start, mut scan_from) = (0usize, 0usize);
        while scan_from + self.window / 2 <= len {
            let end = (start + SEGMENT).min(len);
            file.read_exact_at(&mut buf[..end - start], start as u64)?;
            self.segment(&buf[..end - start], start, scan_from, &mut runs, &mut hits)?;
            if end == len {
                break;
            }
            scan_from = end - self.window / 2 + 1;
            start = end - OVERLAP;
            runs.covered.retain(|_, reach| *reach > start);
            runs.spans.clear();
        }
        Ok(hits)
    }

    fn small_file(&self, bytes: &[u8], hits: &mut FileHits) {
        if bytes.len() >= self.small_min
            && distinct(bytes) >= self.min_distinct.min(bytes.len())
            && let Some(&blob) = self.small.get(bytes)
        {
            // Too short for the aligned public index to be sure of: only pins exempt it.
            let h = chunk_hash(bytes);
            if !self.pins.contains(&h) {
                hits.insert(blob, (1, bytes.len(), vec![h]));
            }
        }
    }

    /// Scans the private windows starting at `from..` of `bytes`, which holds the private file's
    /// bytes from offset `base`.
    fn segment(
        &self,
        bytes: &[u8],
        base: usize,
        from: usize,
        runs: &mut Runs,
        hits: &mut FileHits,
    ) -> io::Result<()> {
        let c = self.window / 2;
        for (k, h) in rolling(bytes, c) {
            let at = base + k;
            if at < from {
                continue;
            }
            let candidates = self.index.candidates(h);
            if candidates.len() > CANDIDATE_CAP {
                self.counters.capped.fetch_add(1, Ordering::Relaxed);
            }
            for &key in candidates.iter().take(CANDIDATE_CAP) {
                let q = key & 0xffff_ffff;
                let diagonal = q as i64 - at as i64;
                if runs.covered.get(&diagonal).is_some_and(|&reach| at + c <= reach) {
                    continue;
                }
                if self.pushed.bytes(q, c) != &bytes[k..k + c] {
                    continue;
                }
                let blob = self.pushed.blob_at(q);
                let (lo, hi) = self.pushed.bounds(blob);
                let left = self.extend(bytes, k, q, lo, true);
                let right = self.extend(bytes, k + c, q + c as u64, hi, false);
                let (start, end) = (k - left, k + c + right);
                runs.covered.insert(diagonal, base + end);
                self.count(&bytes[start..end], base + start, blob, &mut runs.spans, hits);
            }
        }
        Ok(())
    }

    /// How far the private bytes at `k` and the pushed bytes at `q` agree, leftwards from just
    /// before them or rightwards from them, up to the pushed blob's bound and `EXTEND_CAP`.
    fn extend(&self, bytes: &[u8], k: usize, q: u64, bound: u64, left: bool) -> usize {
        if left {
            let room = k.min((q - bound) as usize).min(EXTEND_CAP);
            let theirs = self.pushed.bytes(q - room as u64, room);
            bytes[k - room..k]
                .iter()
                .rev()
                .zip(theirs.iter().rev())
                .take_while(|(a, b)| a == b)
                .count()
        } else {
            let room = (bytes.len() - k).min((bound - q) as usize).min(EXTEND_CAP);
            let theirs = self.pushed.bytes(q, room);
            bytes[k..k + room].iter().zip(theirs).take_while(|(a, b)| a == b).count()
        }
    }

    /// Counts the shared run `run` (at private offset `at`) for `blob` unless it is exempt.
    fn count(
        &self,
        run: &[u8],
        at: usize,
        blob: usize,
        spans: &mut HashMap<usize, Vec<(usize, usize)>>,
        hits: &mut FileHits,
    ) {
        let c = self.window / 2;
        let (lo, hi) = (at, at + run.len());
        let seen = spans.entry(blob).or_default();
        if seen.iter().any(|&(s, e)| s <= lo && hi <= e) {
            return;
        }
        if seen.len() == SPANS_KEPT {
            seen.remove(0);
        }
        seen.push((lo, hi));
        if run.len() < self.window || distinct(run) < self.min_distinct {
            return;
        }
        if let Some((spool, index)) = self.public
            && spool.contains(index, run, c)
        {
            return;
        }
        // The private chunks at multiples of c wholly inside the run.
        let first = at.div_ceil(c) * c - at;
        let chunks: Vec<u64> = (first..)
            .step_by(c)
            .take_while(|&s| s + c <= run.len())
            .map(|s| chunk_hash(&run[s..s + c]))
            .collect();
        if !chunks.is_empty() && chunks.iter().all(|h| self.pins.contains(h)) {
            return;
        }
        let e = hits.entry(blob).or_insert((0, 0, Vec::new()));
        e.0 += 1;
        e.1 = e.1.max(run.len());
        e.2.extend(chunks);
    }
}

fn read_lines(path: &Path) -> io::Result<Vec<(usize, String)>> {
    Ok(fs::read_to_string(path)?
        .lines()
        .enumerate()
        .map(|(i, l)| (i + 1, l.trim().to_string()))
        .filter(|(_, l)| !l.is_empty() && !l.starts_with('#'))
        .collect())
}

fn fail(what: &str, e: impl std::fmt::Display) -> ! {
    eprintln!("fork-push-fp: {what}: {e}");
    std::process::exit(2);
}

fn main() {
    let opts = parse_args();
    let c = opts.window / 2;
    fs::create_dir_all(&opts.spool).unwrap_or_else(|e| fail("creating --spool", e));
    let pushed = spool(&opts.blobs, Some(&opts.repo), &opts.spool.join("pushed"))
        .unwrap_or_else(|e| fail("reading the pushed blobs", e));
    let index = pushed.index(c, c / 2, opts.min_distinct);
    let public = opts.public.as_ref().map(|list| {
        let p = spool(list, None, &opts.spool.join("public"))
            .unwrap_or_else(|e| fail("reading the public blobs", e));
        let i = p.index(c, c, 1);
        (p, i)
    });
    let public_ids: HashSet<[u8; 20]> = match &opts.public_ids {
        Some(path) => read_lines(path)
            .unwrap_or_else(|e| fail("reading --public-ids", e))
            .iter()
            .map(|(_, l)| parse_hex(l).unwrap_or_else(|| fail("a --public-ids line", l)))
            .collect(),
        None => HashSet::new(),
    };
    let exclude = match &opts.exclude {
        Some(path) => read_lines(path).unwrap_or_else(|e| fail("reading --exclude", e)),
        None => Vec::new(),
    };
    let pins: HashSet<u64> = match &opts.pins {
        Some(path) => read_lines(path)
            .unwrap_or_else(|e| fail("reading --pins", e))
            .iter()
            .map(|(_, l)| u64::from_str_radix(l, 16).unwrap_or_else(|e| fail("a --pins line", e)))
            .collect(),
        None => HashSet::new(),
    };
    if !opts.private.is_dir() {
        fail("--private", format!("{} is not a directory", opts.private.display()));
    }
    let mut walk = Walk {
        root: &opts.private,
        exclude: &exclude,
        files: Vec::new(),
        dropped: vec![0; exclude.len()],
        symlinks: 0,
        vanished: 0,
        errors: Vec::new(),
    };
    walk.dir(&opts.private, None);
    let mut small = HashMap::new();
    for blob in 0..pushed.paths.len() {
        let (lo, hi) = pushed.bounds(blob);
        if hi - lo < opts.window as u64 {
            small.entry(pushed.blob(blob).to_vec()).or_insert(blob);
        }
    }
    let scan = Scan {
        window: opts.window,
        min_distinct: opts.min_distinct,
        small_min: opts.small_min,
        pushed: &pushed,
        index: &index,
        small,
        public: public.as_ref().map(|(p, i)| (p, i)),
        public_ids,
        pins,
        counters: Counters::default(),
    };
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<(usize, FileHits)>> = Mutex::new(Vec::new());
    let late: Mutex<(usize, Vec<String>)> = Mutex::new((0, Vec::new()));
    std::thread::scope(|s| {
        for _ in 0..opts.threads {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(path) = walk.files.get(i) else {
                        break;
                    };
                    match scan.file(path) {
                        Ok(hits) if hits.is_empty() => {}
                        Ok(hits) => {
                            results.lock().unwrap_or_else(|e| e.into_inner()).push((i, hits))
                        }
                        Err(e) if e.kind() == io::ErrorKind::NotFound => {
                            late.lock().unwrap_or_else(|e| e.into_inner()).0 += 1;
                        }
                        Err(e) => {
                            late.lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .1
                                .push(format!("{}: {e}", path.display()));
                        }
                    }
                }
            });
        }
    });
    let (late_vanished, late_errors) = late.into_inner().unwrap_or_else(|e| e.into_inner());
    walk.vanished += late_vanished;
    walk.errors.extend(late_errors);
    let results = results.into_inner().unwrap_or_else(|e| e.into_inner());
    let mut lines = Vec::new();
    let mut new_pins = HashSet::new();
    for (i, hits) in &results {
        let path = &walk.files[*i];
        let rel = path.strip_prefix(&opts.private).unwrap_or(path).display().to_string();
        for (&blob, (runs, longest, chunks)) in hits {
            new_pins.extend(chunks.iter().copied());
            for pushed_path in &pushed.paths[blob] {
                lines.push(format!("{pushed_path}\t{rel}\t{longest}\t{runs}"));
            }
        }
    }
    lines.sort();
    let mut status = 0;
    if let Err(e) = fs::write(&opts.out, lines.iter().map(|l| format!("{l}\n")).collect::<String>())
    {
        eprintln!("fork-push-fp: writing {}: {e}", opts.out.display());
        status = 2;
    }
    if let Some(path) = &opts.write_pins {
        let mut sorted: Vec<u64> = new_pins.into_iter().collect();
        sorted.sort_unstable();
        let text: String = sorted.iter().map(|h| format!("{h:016x}\n")).collect();
        if let Err(e) = fs::write(path, text) {
            eprintln!("fork-push-fp: writing {}: {e}", path.display());
            status = 2;
        }
    }
    let counters = &scan.counters;
    eprintln!(
        "fork-push-fp: {} pushed blobs ({} bytes, {} index entries), {} public blobs, {} private files \
         ({} bytes), {} skipped as public blobs, {} symlinks skipped, {} vanished, {} errors, \
         {} capped lookups, {} hits",
        pushed.paths.len(),
        pushed.len(),
        index.keys.len(),
        public.as_ref().map_or(0, |(p, _)| p.paths.len()),
        walk.files.len(),
        counters.scanned_bytes.load(Ordering::Relaxed),
        counters.public_files.load(Ordering::Relaxed),
        walk.symlinks,
        walk.vanished,
        walk.errors.len(),
        counters.capped.load(Ordering::Relaxed),
        lines.len()
    );
    for ((line, _), dropped) in exclude.iter().zip(&walk.dropped) {
        eprintln!("fork-push-fp: scope line {line}: {dropped} files not read");
    }
    for e in walk.errors.iter().take(20) {
        eprintln!("fork-push-fp: error: {e}");
    }
    if status == 0 && !walk.errors.is_empty() {
        status = 2;
    }
    if status == 0 && !lines.is_empty() {
        status = 1;
    }
    std::process::exit(status);
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Rng(u64);

    impl Rng {
        fn bytes(&mut self, n: usize) -> Vec<u8> {
            (0..n)
                .map(|_| {
                    self.0 ^= self.0 << 13;
                    self.0 ^= self.0 >> 7;
                    self.0 ^= self.0 << 17;
                    (self.0 >> 24) as u8
                })
                .collect()
        }
    }

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("fork-push-fp-test-{}-{name}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A spool of `blobs` given as files.
    fn spool_of(name: &str, blobs: &[Vec<u8>]) -> Spool {
        let dir = temp(name);
        let list = dir.join("list");
        let mut text = String::new();
        for (i, b) in blobs.iter().enumerate() {
            let f = dir.join(format!("blob{i}"));
            fs::write(&f, b).unwrap();
            text.push_str(&format!("file:{}\tblob{i}\n", f.display()));
        }
        fs::write(&list, text).unwrap();
        spool(&list, Some(&dir), &dir.join("spool")).unwrap()
    }

    /// Hits (pushed blob -> runs) of private `bytes` against pushed `blobs`.
    fn hits(
        name: &str,
        blobs: &[Vec<u8>],
        public: &[Vec<u8>],
        bytes: &[u8],
    ) -> HashMap<usize, usize> {
        let pushed = spool_of(name, blobs);
        let index = pushed.index(32, 16, 4);
        let public = (!public.is_empty()).then(|| {
            let p = spool_of(&format!("{name}-public"), public);
            let i = p.index(32, 32, 1);
            (p, i)
        });
        let scan = Scan {
            window: 64,
            min_distinct: 4,
            small_min: 16,
            pushed: &pushed,
            index: &index,
            small: HashMap::new(),
            public: public.as_ref().map(|(p, i)| (p, i)),
            public_ids: HashSet::new(),
            pins: HashSet::new(),
            counters: Counters::default(),
        };
        let dir = temp(&format!("{name}-private"));
        fs::write(dir.join("file"), bytes).unwrap();
        let out = scan.file(&dir.join("file")).unwrap();
        fs::remove_dir_all(temp(name)).ok();
        fs::remove_dir_all(dir).ok();
        out.into_iter().map(|(b, (runs, _, _))| (b, runs)).collect()
    }

    #[test]
    fn an_exact_64_byte_run_at_every_alignment_and_edge() {
        let mut rng = Rng(7);
        let run = rng.bytes(64);
        for private_at in 0..32 {
            for pushed_at in [0usize, 1, 15, 16, 17, 31] {
                let mut private = rng.bytes(private_at);
                private.extend(&run);
                private.extend(rng.bytes(5));
                let mut blob = rng.bytes(pushed_at);
                blob.extend(&run);
                blob.extend(rng.bytes(3));
                let h = hits(&format!("align-{private_at}-{pushed_at}"), &[blob], &[], &private);
                assert_eq!(h.get(&0), Some(&1), "private at {private_at}, pushed at {pushed_at}");
            }
        }
        // The run is the whole of both files.
        assert_eq!(hits("whole", std::slice::from_ref(&run), &[], &run).get(&0), Some(&1));
        // 63 bytes are below the window.
        assert!(hits("short", &[run[..63].to_vec()], &[], &run[..63]).is_empty());
    }

    #[test]
    fn an_earlier_partial_match_does_not_hide_a_later_copy() {
        // Review M-3, probe A: the blob first holds a 32-byte piece of the private run, then the run.
        let mut rng = Rng(11);
        let l = rng.bytes(64);
        let mut private = rng.bytes(16);
        private.extend(&l);
        private.extend(rng.bytes(32));
        let mut blob = rng.bytes(50);
        blob.extend(&l[16..48]);
        blob.extend(rng.bytes(50));
        blob.extend(&l);
        blob.extend(rng.bytes(50));
        assert_eq!(hits("probe-a", &[blob], &[], &private).get(&0), Some(&1));
    }

    #[test]
    fn a_public_copy_does_not_hide_a_private_one_in_the_same_blob() {
        let mut rng = Rng(13);
        let (public_run, private_run) = (rng.bytes(80), rng.bytes(80));
        let mut private = public_run.clone();
        private.extend(rng.bytes(7));
        private.extend(&private_run);
        let mut blob = public_run.clone();
        blob.extend(rng.bytes(9));
        blob.extend(&private_run);
        assert_eq!(hits("public-first", &[blob], &[public_run], &private).get(&0), Some(&1));
    }

    #[test]
    fn a_run_whose_private_aligned_chunk_is_zeros() {
        // Review m-2, probe B: 16 random bytes, 32 zeros at a private multiple of 32, 16 random.
        let mut rng = Rng(17);
        let mut run = rng.bytes(16);
        run.extend([0u8; 32]);
        run.extend(rng.bytes(16));
        let mut private = rng.bytes(16);
        private.extend(&run);
        private.extend(rng.bytes(16));
        for pushed_at in [0usize, 5, 16, 23] {
            let mut blob = rng.bytes(pushed_at);
            blob.extend(&run);
            assert_eq!(
                hits(&format!("probe-b-{pushed_at}"), &[blob], &[], &private).get(&0),
                Some(&1)
            );
        }
    }

    #[test]
    fn a_run_longer_than_a_segment_boundary_is_found_once_per_diagonal() {
        let mut rng = Rng(19);
        let run = rng.bytes(4096);
        let blob = run.clone();
        let mut private = vec![b'a'; SEGMENT - 1000];
        private.extend(&run);
        private.extend(vec![b'b'; 5000]);
        let h = hits("segments", &[blob], &[], &private);
        assert!(h.get(&0).is_some_and(|&runs| (1..=2).contains(&runs)), "{h:?}");
    }

    #[test]
    fn low_distinct_runs_and_public_runs_are_exempt() {
        let mut rng = Rng(23);
        let repeat: Vec<u8> = b"abc".iter().cycle().take(200).copied().collect();
        assert!(hits("low", std::slice::from_ref(&repeat), &[], &repeat).is_empty());
        let run = rng.bytes(100);
        assert!(
            hits("public", std::slice::from_ref(&run), std::slice::from_ref(&run), &run).is_empty()
        );
    }

    #[test]
    fn git_blob_ids() {
        // `git hash-object` of the empty file and of "hello\n".
        let hex = |id: [u8; 20]| id.iter().map(|b| format!("{b:02x}")).collect::<String>();
        assert_eq!(hex(git_blob_id(b"")), "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391");
        assert_eq!(hex(git_blob_id(b"hello\n")), "ce013625030ba8dba906f756967f9e9ca394464a");
        assert_eq!(parse_hex::<2>("0aff"), Some([0x0a, 0xff]));
    }
}
