//! Reading StateDigest files and the stored recordings (node program task O9a).
//!
//! This module is the only code that knows the storage layout of spec O8 §13.1
//! (`test-vectors/statedigest/<set>/<test>/[manifest.json]`, `node<i>/{digest,decisions,golden}
//! .jsonl.zst`, `extract.json`). O8 is still settling it (the manifest and the hashes that bind
//! `extract.json` to the files are new), so everything that depends on it stays here.
//!
//! Files are read decompressed: a file that starts with a zstd frame (or a skippable frame) is
//! decoded as a stream by libzstd (the `zstd` crate over the `zstd-sys` RocksDB brings in),
//! whatever its name: every frame to the end of the file, skippable frames skipped, checksums
//! checked. Any other file is read as is.

use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use dash_statedigest::{Record, parse_line};
use dashcore::hashes::{Hash as _, HashEngine as _, sha256};

const ZSTD_MAGIC: u32 = 0xfd2f_b528;
/// Skippable frames have the magics `0x184d2a50..=0x184d2a5f`.
const SKIPPABLE_MAGIC: u32 = 0x184d_2a50;

fn is_zstd(head: &[u8]) -> bool {
    let Some(magic) = head.first_chunk::<4>().map(|m| u32::from_le_bytes(*m)) else {
        return false;
    };
    magic == ZSTD_MAGIC || magic & !0xf == SKIPPABLE_MAGIC
}

/// Opens a file for reading, decompressing it if it is zstd.
pub fn open(path: &Path) -> Result<Box<dyn BufRead + Send>> {
    let mut file =
        BufReader::new(File::open(path).with_context(|| format!("opening {}", path.display()))?);
    let head = file.fill_buf().with_context(|| format!("reading {}", path.display()))?;
    if is_zstd(head) {
        let decoder = zstd::stream::read::Decoder::with_buffer(file)
            .with_context(|| format!("opening the zstd stream of {}", path.display()))?;
        Ok(Box::new(BufReader::new(decoder)))
    } else {
        Ok(Box::new(file))
    }
}

/// The file's (decompressed) bytes.
pub fn read(path: &Path) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    open(path)?.read_to_end(&mut out).with_context(|| format!("reading {}", path.display()))?;
    Ok(out)
}

/// The SHA-256 of `bytes`, as lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    sha256::Hash::hash(bytes).to_string()
}

/// The SHA-256 of the file's (decompressed) bytes, read as a stream.
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut input = open(path)?;
    let mut engine = sha256::Hash::engine();
    loop {
        let chunk = input.fill_buf().with_context(|| format!("reading {}", path.display()))?;
        if chunk.is_empty() {
            break;
        }
        engine.input(chunk);
        let n = chunk.len();
        input.consume(n);
    }
    Ok(sha256::Hash::from_engine(engine).to_string())
}

/// The lines of a file, decompressed and numbered from 1. A read error ends the iteration.
pub struct RawLines {
    name: String,
    inner: Box<dyn BufRead + Send>,
    line: usize,
    done: bool,
}

impl Iterator for RawLines {
    type Item = Result<(usize, Vec<u8>)>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let mut buf = Vec::new();
        match self.inner.read_until(b'\n', &mut buf) {
            Ok(0) => {
                self.done = true;
                None
            }
            Ok(_) => {
                self.line += 1;
                Some(Ok((self.line, buf)))
            }
            Err(e) => {
                self.done = true;
                Some(Err(anyhow::Error::new(e).context(format!("reading {}", self.name))))
            }
        }
    }
}

/// The records of a file, streamed: `(line number, record)`. A line that does not parse is an
/// error naming the file and line, and ends the iteration.
pub struct Records(RawLines);

impl Records {
    pub fn open(path: &Path) -> Result<Records> {
        Records::raw(path).map(Records)
    }

    /// The raw lines with their numbers, unparsed.
    pub fn raw(path: &Path) -> Result<RawLines> {
        Ok(RawLines {
            name: path.display().to_string(),
            inner: open(path)?,
            line: 0,
            done: false,
        })
    }
}

impl Iterator for Records {
    type Item = Result<(usize, Record)>;

    fn next(&mut self) -> Option<Self::Item> {
        let (line, bytes) = match self.0.next()? {
            Ok(item) => item,
            Err(e) => return Some(Err(e)),
        };
        Some(parse_line(&bytes).map(|r| (line, r)).map_err(|e| {
            self.0.done = true;
            anyhow::anyhow!("{}:{line}: {e}", self.0.name)
        }))
    }
}

/// A JSON file.
fn read_json(path: &Path) -> Result<serde_json::Value> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))
}

/// A set of stored recordings, e.g. `test-vectors/statedigest/regtest-v24`.
#[derive(Clone, Debug)]
pub struct Vectors {
    pub root: PathBuf,
}

/// One recorded node of a stored set.
#[derive(Clone, Debug)]
pub struct Node {
    pub test: String,
    /// `node<i>`.
    pub name: String,
    pub dir: PathBuf,
}

impl Vectors {
    /// The regtest v24 set in this repository.
    pub fn regtest_v24() -> Vectors {
        Vectors {
            root: Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../test-vectors/statedigest/regtest-v24"),
        }
    }

    /// The recorded tests, sorted.
    pub fn tests(&self) -> Result<Vec<String>> {
        let mut tests = Vec::new();
        for entry in std::fs::read_dir(&self.root)
            .with_context(|| format!("listing {}", self.root.display()))?
        {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                tests.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
        tests.sort();
        Ok(tests)
    }

    /// A test's recorded nodes in index order: the manifest's `recorded` list when the test has
    /// a `manifest.json`, else every `node<i>` directory. Either way each node's directory must
    /// exist.
    pub fn nodes(&self, test: &str) -> Result<Vec<Node>> {
        let dir = self.root.join(test);
        let manifest = dir.join("manifest.json");
        let mut names: Vec<String> = if manifest.exists() {
            read_json(&manifest)?["recorded"]
                .as_array()
                .with_context(|| format!("{}: no recorded list", manifest.display()))?
                .iter()
                .map(|v| v.as_str().map(str::to_owned).context("a node name that is not text"))
                .collect::<Result<_>>()?
        } else {
            let mut names = Vec::new();
            for entry in
                std::fs::read_dir(&dir).with_context(|| format!("listing {}", dir.display()))?
            {
                let name = entry?.file_name().to_string_lossy().into_owned();
                if node_index(&name).is_some() {
                    names.push(name);
                }
            }
            names
        };
        names.sort_by_key(|n| node_index(n));
        names
            .into_iter()
            .map(|name| {
                let node_dir = dir.join(&name);
                if !node_dir.is_dir() {
                    bail!("{}: recorded node {name} has no directory", dir.display());
                }
                Ok(Node {
                    test: test.to_owned(),
                    name,
                    dir: node_dir,
                })
            })
            .collect()
    }
}

fn node_index(name: &str) -> Option<u32> {
    name.strip_prefix("node")?.parse().ok()
}

impl Node {
    fn file(&self, stem: &str) -> PathBuf {
        let zst = self.dir.join(format!("{stem}.zst"));
        if zst.exists() {
            zst
        } else {
            self.dir.join(stem)
        }
    }

    /// `digest.jsonl(.zst)`.
    pub fn digest(&self) -> PathBuf {
        self.file("digest.jsonl")
    }

    /// `decisions.jsonl(.zst)`.
    pub fn decisions(&self) -> PathBuf {
        self.file("decisions.jsonl")
    }

    /// `golden.jsonl(.zst)`.
    pub fn golden(&self) -> PathBuf {
        self.file("golden.jsonl")
    }

    /// `blocks.dat(.zst)`: every block the node had at the end, the replay input (O8 §13.1).
    pub fn blocks(&self) -> PathBuf {
        self.file("blocks.dat")
    }

    /// `test/nodeN`, for messages.
    pub fn label(&self) -> String {
        format!("{}/{}", self.test, self.name)
    }

    /// Checks the decompressed digest, decisions, golden and blocks.dat files against the hashes
    /// `extract.json` recorded for them (O8 §13.1) and returns the files it checked. All four
    /// must be stored and hashed: a missing file, a missing hash and a hash that differs each
    /// fail.
    pub fn verify(&self) -> Result<Vec<&'static str>> {
        self.verify_against(&read_json(&self.dir.join("extract.json"))?)
    }

    /// [`Node::verify`] against an `extract.json` the caller has parsed.
    pub fn verify_against(&self, json: &serde_json::Value) -> Result<Vec<&'static str>> {
        let mut checked = self.verify_records_against(json)?;
        checked.extend(self.check_hashes([(
            "blocks.dat",
            self.blocks(),
            &json["blocks_dat_sha256"],
        )])?);
        Ok(checked)
    }

    /// [`Node::verify_against`] for the digest, decisions and golden only: a replay checks
    /// `blocks.dat` in its streaming pre-pass, which also reads its blocks (CH3a D148).
    pub fn verify_records_against(&self, json: &serde_json::Value) -> Result<Vec<&'static str>> {
        self.check_hashes([
            ("digest", self.digest(), &json["digest"]["sha256"]),
            ("decisions", self.decisions(), &json["decisions"]["sha256"]),
            ("golden", self.golden(), &json["golden"]["sha256"]),
        ])
    }

    fn check_hashes<const N: usize>(
        &self,
        files: [(&'static str, PathBuf, &serde_json::Value); N],
    ) -> Result<Vec<&'static str>> {
        let mut checked = Vec::new();
        for (key, path, want) in files {
            let Some(want) = want.as_str() else {
                bail!("{}: extract.json has no sha256 for {key}", self.label());
            };
            if !path.exists() {
                bail!("{}: {key} is not stored", self.label());
            }
            let have = sha256_file(&path)?;
            if have != want {
                bail!("{}: sha256 {have}, extract.json says {want}", path.display());
            }
            checked.push(key);
        }
        Ok(checked)
    }
}

#[cfg(test)]
mod tests {
    //! The zstd reading on frames libzstd writes (synthetic content, not vectors).

    use std::path::Path;

    use super::{is_zstd, read};

    fn frame(content: &[u8]) -> Vec<u8> {
        zstd::bulk::Compressor::new(1)
            .and_then(|mut c| {
                c.include_checksum(true)?;
                c.compress(content)
            })
            .unwrap()
    }

    /// `bytes` read back through [`read`] from a file.
    fn decode(bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("input");
        std::fs::write(&path, bytes).unwrap();
        read(Path::new(&path))
    }

    /// A skippable frame holding `content`.
    fn skippable(content: &[u8]) -> Vec<u8> {
        let mut out = 0x184d_2a5eu32.to_le_bytes().to_vec();
        out.extend((content.len() as u32).to_le_bytes());
        out.extend(content);
        out
    }

    #[test]
    fn every_frame_is_decoded_and_skippable_frames_are_skipped() {
        let (a, b) = (b"first frame\n".repeat(1000), b"second frame\n".repeat(10));
        let mut bytes = skippable(b"index");
        bytes.extend(frame(&a));
        bytes.extend(skippable(b""));
        bytes.extend(frame(&b));
        assert!(is_zstd(&bytes));
        assert_eq!(decode(&bytes).unwrap(), [a, b].concat());
        // A file that is not zstd is read as is.
        assert_eq!(decode(b"{\"k\":\"H\"}\n").unwrap(), b"{\"k\":\"H\"}\n");
    }

    #[test]
    fn a_damaged_stream_fails() {
        let content = b"some content\n".repeat(100);
        let good = frame(&content);
        assert_eq!(decode(&good).unwrap(), content);
        // The frame carries a content checksum (the descriptor's bit 2; its last four bytes):
        // flipping it fails.
        assert_ne!(good[4] & 0x04, 0);
        let mut bad = good.clone();
        *bad.last_mut().unwrap() ^= 1;
        assert!(decode(&bad).is_err());
        // A cut frame, a cut skippable frame and trailing garbage fail too.
        assert!(decode(&good[..good.len() - 5]).is_err());
        assert!(decode(&skippable(b"index")[..9]).is_err());
        assert!(decode(&[good.as_slice(), b"garbage"].concat()).is_err());
    }

    /// A single-segment frame of one raw block holding `content` (at most 255 bytes), whose
    /// header declares `declared` bytes and no checksum (RFC 8878 §3.1.1).
    fn raw_frame(content: &[u8], declared: u8) -> Vec<u8> {
        let mut out = vec![0x28, 0xb5, 0x2f, 0xfd, 0x20, declared];
        let block = 1 | (content.len() as u32) << 3; // last block, raw, its size
        out.extend(&block.to_le_bytes()[..3]);
        out.extend(content);
        out
    }

    #[test]
    fn a_frame_shorter_or_longer_than_its_declared_size_fails() {
        let content = b"sized content\n".repeat(10);
        let len = content.len() as u8;
        assert_eq!(decode(&raw_frame(&content, len)).unwrap(), content);
        assert!(decode(&raw_frame(&content, len + 1)).is_err());
        assert!(decode(&raw_frame(&content, len - 1)).is_err());
    }

    #[test]
    fn only_zstd_and_skippable_magics_are_zstd() {
        assert!(is_zstd(&0xfd2f_b528u32.to_le_bytes()));
        assert!(is_zstd(&0x184d_2a50u32.to_le_bytes()));
        assert!(is_zstd(&0x184d_2a5fu32.to_le_bytes()));
        assert!(!is_zstd(&0x184d_2a60u32.to_le_bytes()));
        assert!(!is_zstd(&0x184d_2a4fu32.to_le_bytes()));
        assert!(!is_zstd(b"{\"k\":\"H\"}"));
        assert!(!is_zstd(&[0x28, 0xb5, 0x2f]));
    }
}
