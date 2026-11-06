//! Differential test of the external block-file scanner and the block-file writer against a real
//! dashd, on damaged inputs.
//!
//! Each case takes the first blocks of the `regtest-40000` dataset, writes them as a bootstrap
//! file, damages it with seeded random edits (garbage, deletions, partial magics, bad size fields,
//! duplicated and swapped records, flipped transaction bytes, truncation, zero padding), and
//! imports it twice:
//! - `dashd -loadblock=<file> -stopafterblockimport` on a fresh regtest datadir;
//! - [`load_external_block_file`] with a model of Core's block index, then `BlockFiles` writing
//!   the accepted blocks and the undo data of the connected chain.
//!
//! The resulting blk and rev files must be byte-identical: same blocks found, in the same order,
//! at the same positions, with the same chain connected. Every other case runs with `-fastprune`
//! (64 KiB block files) to exercise rollover.
//!
//! Large cases use the whole dataset (about 39 MB, many `CBufferedFile` refills) with the damage
//! placed around Core's buffer fill boundaries.
//!
//! Needs `DASHD_TEST_DATA` and a v24 dashd: `ST2_DASHD` (colon-separated binaries; when set, a
//! missing dataset or v24 binary fails the test) or `DASHD_PATH` (skipped, visibly, if not v24).
//! `ST2_SCAN_DIFF_CASES` and `ST2_SCAN_DIFF_LARGE` set the number of small (default 24) and large
//! (default 2) cases per dashd, and `ST2_SCRATCH` the directory for the dashd datadirs (default:
//! cargo's target tmp dir; a tmpfs is much faster).

mod common;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;
use std::time::Duration;

use common::{
    REGTEST_MAGIC as MAGIC, block_time, compare_dirs, core_bytes, is_v24, prev_hash, undo_for_chain,
};
use dash_coreimport::Hash256;
use dash_coreimport::blockfiles::external::{
    ExternalBlockSink, Flow, HeaderStatus, load_external_block_file,
};
use dash_coreimport::blockfiles::scan_block_records;
use dash_coreimport::tx::block_hash;
use dashd_store::flatfile::{
    BlockFiles, BlockFilesConfig, BlockFilesState, CoreBytes, CoreUndoBytes, OsFs,
};

/// Blocks in the small cases' source chain (the large cases use the whole dataset).
const SMALL_CHAIN: usize = 401;
/// Core's first `CBufferedFile` fill (buffer minus rewind reserve) and its buffer size: damage near
/// their multiples exercises refills, ring wrap-around and rewinds across a fill.
const FILL_BOUNDARIES: [usize; 2] = [2 * 2_000_000 - (2_000_000 + 8), 2 * 2_000_000];

/// splitmix64.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// The source chain: blocks by height.
struct Chain {
    blocks: Vec<CoreBytes>,
    genesis: Hash256,
    by_hash: HashMap<Hash256, usize>,
    undo: Vec<CoreUndoBytes>,
}

fn source_chain(data: &Path, len: usize) -> Chain {
    let path = data.join("regtest-40000/regtest/blocks/blk00000.dat");
    let file = std::fs::read(&path).unwrap();
    let (records, _) = scan_block_records(&file, MAGIC);
    // The dataset was mined in order: file order is height order.
    let blocks: Vec<CoreBytes> =
        records.iter().take(len).map(|r| core_bytes(&file[r.offset..r.offset + r.len])).collect();
    let hashes: Vec<Hash256> = blocks.iter().map(|b| block_hash(&b.as_slice()[..80])).collect();
    for h in 1..blocks.len() {
        assert_eq!(
            prev_hash(blocks[h].as_slice()),
            hashes[h - 1],
            "the dataset is not in height order"
        );
    }
    Chain {
        undo: undo_for_chain(blocks.iter().map(CoreBytes::as_slice)),
        genesis: hashes[0],
        by_hash: hashes.iter().enumerate().map(|(h, hash)| (*hash, h)).collect(),
        blocks,
    }
}

fn record(block: &[u8]) -> Vec<u8> {
    let mut r = MAGIC.to_vec();
    r.extend((block.len() as u32).to_le_bytes());
    r.extend(block);
    r
}

/// A damaged bootstrap file. Byte-level damage lands anywhere, or with `near_fills` close to
/// multiples of Core's buffer fill boundaries (and the file is not truncated).
fn damaged(chain: &Chain, seed: u64, near_fills: bool) -> Vec<u8> {
    let mut rng = Rng(seed);
    let mut recs: Vec<Vec<u8>> = chain.blocks.iter().map(|b| record(b.as_slice())).collect();
    for _ in 0..1 + rng.below(3) {
        let i = rng.below(recs.len());
        match rng.below(4) {
            0 => {
                let j = rng.below(recs.len());
                let dup = recs[i].clone();
                recs.insert(j, dup);
            }
            1 => {
                let j = rng.below(recs.len());
                recs.swap(i, j);
            }
            2 => {
                let size = u32::from_le_bytes(recs[i][4..8].try_into().unwrap());
                let new = match rng.below(5) {
                    0 => rng.below(80) as u32,
                    1 => size.saturating_sub(1 + rng.below(50) as u32),
                    2 => size + 1 + rng.below(300) as u32,
                    3 => 2_000_001 + rng.below(1000) as u32,
                    _ => rng.next() as u32,
                };
                recs[i][4..8].copy_from_slice(&new.to_le_bytes());
            }
            _ => {
                // A transaction byte (the header is left intact so no valid sibling block appears).
                let len = recs[i].len();
                if len > 89 {
                    let at = 88 + rng.below(len - 88);
                    recs[i][at] ^= 1 << rng.below(8);
                }
            }
        }
    }
    let mut file: Vec<u8> = recs.concat();
    let byte_ops = if near_fills {
        2 + rng.below(4)
    } else {
        rng.below(4)
    };
    for _ in 0..byte_ops {
        let at = if near_fills {
            let unit = FILL_BOUNDARIES[rng.below(2)];
            let k = 1 + rng.below((file.len() / unit).max(1));
            (k * unit + rng.below(600)).saturating_sub(300).min(file.len())
        } else {
            rng.below(file.len() + 1)
        };
        match rng.below(if near_fills {
            3
        } else {
            5
        }) {
            0 => {
                let garbage: Vec<u8> = (0..1 + rng.below(64))
                    .map(|_| {
                        if rng.below(4) == 0 {
                            MAGIC[0]
                        } else {
                            rng.next() as u8
                        }
                    })
                    .collect();
                file.splice(at..at, garbage);
            }
            1 => {
                let end = (at + 1 + rng.below(32)).min(file.len());
                file.drain(at..end);
            }
            2 => {
                let k = 1 + rng.below(3);
                file.splice(at..at, MAGIC[..k].iter().copied());
            }
            3 => file.truncate(file.len() - rng.below(file.len() / 4 + 1)),
            _ => file.extend(std::iter::repeat_n(0u8, 1 + rng.below(5000))),
        }
    }
    file
}

/// Core's block index and chain during `-loadblock`, for the source chain's blocks; stores and
/// connects blocks with `BlockFiles` as Core would.
struct ImportModel<'a> {
    chain: &'a Chain,
    /// Hashes with a block index entry (header accepted).
    index: HashSet<Hash256>,
    /// Blk file of every stored block, by height (`BLOCK_HAVE_DATA`).
    stored: HashMap<usize, u32>,
    /// The active chain's height; `None` until genesis is activated.
    tip: Option<usize>,
    files: BlockFiles<OsFs>,
}

impl ImportModel<'_> {
    /// ActivateBestChain: connect the longest stored prefix of the chain.
    fn activate(&mut self) {
        let mut h = self.tip.map_or(0, |t| t + 1);
        while let Some(&file) = self.stored.get(&h) {
            if h > 0 {
                let prev = prev_hash(self.chain.blocks[h].as_slice());
                self.files.write_undo(&self.chain.undo[h], file, h as u32, &prev).unwrap();
            }
            self.tip = Some(h);
            h += 1;
        }
    }
}

impl ExternalBlockSink for ImportModel<'_> {
    fn header(&mut self, header: &[u8; 80], hash: &Hash256, _: u64) -> HeaderStatus {
        if *hash != self.chain.genesis && !self.index.contains(&prev_hash(header)) {
            HeaderStatus::UnknownParent
        } else if self.chain.by_hash.get(hash).is_some_and(|h| self.stored.contains_key(h)) {
            HeaderStatus::Stored
        } else {
            HeaderStatus::New
        }
    }

    fn block(&mut self, block: &[u8], hash: &Hash256, _: u64) -> Flow {
        // AcceptBlock: the header of a source block is valid, so it enters the index even when
        // the transactions were damaged (CheckBlock then fails as mutated, without marking it
        // invalid); only the intact block is stored.
        if let Some(&height) = self.chain.by_hash.get(hash) {
            self.index.insert(*hash);
            let core = &self.chain.blocks[height];
            if core.as_slice() == block {
                let time = block_time(block);
                let pos = self.files.save_block(core, height as u32, time, None).unwrap();
                self.stored.insert(height, pos.file);
            }
        }
        Flow::Continue
    }

    fn after(&mut self, hash: &Hash256) -> Flow {
        // "Activate the genesis block so normal node progress can continue."
        if *hash == self.chain.genesis {
            self.activate();
        }
        Flow::Continue
    }
}

/// What one import did.
#[derive(Debug, Default, Clone, Copy)]
struct Stats {
    stored: usize,
    connected: usize,
    undecodable: usize,
    unexpected: usize,
}

/// Imports `file` the way the test models Core; writes the blocks directory to `out`.
fn import_ours(chain: &Chain, file: &[u8], out: &Path, fastprune: bool) -> Stats {
    let mut cfg = BlockFilesConfig::new(out, MAGIC);
    cfg.fastprune = fastprune;
    let mut model = ImportModel {
        chain,
        index: HashSet::from([chain.genesis]),
        stored: HashMap::new(),
        tip: None,
        files: BlockFiles::with_fs(Arc::new(OsFs::new()), cfg, BlockFilesState::default()),
    };
    // LoadGenesisBlock at startup.
    let genesis = &chain.blocks[0];
    let pos = model.files.save_block(genesis, 0, block_time(genesis.as_slice()), None).unwrap();
    model.stored.insert(0, pos.file);
    let report = load_external_block_file(file, MAGIC, &mut model);
    // ThreadImport activates the best chain after the import.
    model.activate();
    // Shutdown: the production barrier, which must create the empty rev file Core's flush does
    // when the last blk file's blocks were stored but never connected.
    model.files.sync().unwrap();
    Stats {
        stored: model.stored.len() - 1,
        connected: model.tip.unwrap(),
        undecodable: report.undecodable.count as usize,
        unexpected: report.unexpected_data.count as usize,
    }
}

/// What `dashd -version` told about a binary.
#[derive(Debug, PartialEq, Eq)]
enum Probe {
    V24,
    /// A successful run whose version line names another release (the line).
    NotV24(String),
    /// The run said nothing usable: failed to start, failed, or printed no version line. A failed
    /// `-version` prints nothing to stdout (see [`run_version`]), so this is an environment failure,
    /// not an answer about the binary.
    Undetermined {
        status: String,
        stdout: String,
        stderr: String,
    },
}

/// Attempts at `dashd -version` before the answer is `Undetermined`, and the pause after attempt
/// `n` (`n` times this).
const PROBE_ATTEMPTS: u32 = 5;
const PROBE_BACKOFF: Duration = Duration::from_millis(200);

/// Whether `line` is a version line (`Dash Core version v24.0.0-rc.3`): it names "version" and holds
/// a `v<digit>….` release token, so a truncated line is not one.
fn is_version_line(line: &str) -> bool {
    line.contains("version")
        && line.split_whitespace().any(|word| {
            word.strip_prefix('v')
                .is_some_and(|r| r.starts_with(|c: char| c.is_ascii_digit()) && r.contains('.'))
        })
}

/// Runs `dashd -version` with its own empty datadir.
///
/// Even `-version` reads `<datadir>/settings.json` and rewrites it (to `settings.json.tmp`, then a
/// rename). The default datadir, `~/.dashcore`, is shared by every process of the user, so two
/// probes at the same moment (this file's two tests, or another thread's) race on the temp file:
/// the loser exits 1 with empty stdout ("Failed renaming settings file"), and a truncated
/// `settings.json` fails every probe after it.
fn run_version(dashd: &Path) -> std::io::Result<Output> {
    use std::os::unix::fs::PermissionsExt;

    let datadir = tempfile::Builder::new()
        .prefix("st2-version-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()?;
    Command::new(dashd)
        .arg(format!("-datadir={}", datadir.path().display()))
        .arg("-version")
        .output()
}

/// Classifies one run of `dashd -version`: `NotV24` only for a successful run whose version line
/// parses and isn't v24.
fn classify(run: std::io::Result<Output>) -> Probe {
    let out = match run {
        Ok(out) => out,
        Err(e) => {
            return Probe::Undetermined {
                status: format!("failed to run: {e}"),
                stdout: String::new(),
                stderr: String::new(),
            };
        }
    };
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if out.status.success()
        && let Some(line) = stdout.lines().find(|l| is_version_line(l))
    {
        return if is_v24(line) {
            Probe::V24
        } else {
            Probe::NotV24(line.to_owned())
        };
    }
    Probe::Undetermined {
        status: out.status.to_string(),
        stdout,
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// Runs `dashd -version` up to `PROBE_ATTEMPTS` times, until a run is not `Undetermined`.
fn probe_dashd(dashd: &Path, backoff: Duration) -> Probe {
    let mut probe = classify(run_version(dashd));
    for attempt in 1..PROBE_ATTEMPTS {
        if !matches!(probe, Probe::Undetermined { .. }) {
            break;
        }
        std::thread::sleep(backoff * attempt);
        probe = classify(run_version(dashd));
    }
    probe
}

/// Whether `probe` of `dashd` is v24. An undetermined probe panics: a gate must neither read an
/// environment failure as "not v24" (and skip, or blame the binary) nor pass with nothing compared.
fn verdict(dashd: &Path, probe: Probe) -> bool {
    match probe {
        Probe::V24 => true,
        Probe::NotV24(line) => {
            eprintln!("skipped {}: not v24 ({line})", dashd.display());
            false
        }
        Probe::Undetermined {
            status,
            stdout,
            stderr,
        } => panic!(
            "could not determine the version of {} after {PROBE_ATTEMPTS} attempts of `-version`: \
             {status}; stdout {stdout:?}; stderr {stderr:?}",
            dashd.display()
        ),
    }
}

fn dashd_is_v24(dashd: &Path) -> bool {
    verdict(dashd, probe_dashd(dashd, PROBE_BACKOFF))
}

/// The dataset and the v24 dashd binaries, if the environment provides them.
///
/// `ST2_DASHD` is the request for the live differential, so when it is set a missing dataset or a
/// list without a v24 dashd fails the test instead of skipping it: a gate that sets it can't pass
/// with nothing compared. Without it the test skips when `DASHD_PATH` is not v24 (the SDK's
/// integration environment pins v23.1), and says so.
fn environment() -> Option<(PathBuf, Vec<PathBuf>)> {
    let required = std::env::var("ST2_DASHD").ok();
    let Some(data) = std::env::var("DASHD_TEST_DATA").ok() else {
        assert!(required.is_none(), "ST2_DASHD is set but DASHD_TEST_DATA is not");
        eprintln!("skipped: needs DASHD_TEST_DATA and ST2_DASHD or DASHD_PATH");
        return None;
    };
    let Some(dashds) = required.clone().or_else(|| std::env::var("DASHD_PATH").ok()) else {
        eprintln!("skipped: needs DASHD_TEST_DATA and ST2_DASHD or DASHD_PATH");
        return None;
    };
    let v24: Vec<PathBuf> = dashds
        .split(':')
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .filter(|d| dashd_is_v24(d))
        .collect();
    if v24.is_empty() {
        assert!(required.is_none(), "ST2_DASHD ({dashds}) names no v24 dashd");
        eprintln!("skipped: no v24 dashd in DASHD_PATH");
        return None;
    }
    Some((PathBuf::from(data), v24))
}

/// Imports `cases` damaged files per dashd both ways and compares the results.
fn run_differential(
    chain: &Chain,
    dashds: &[PathBuf],
    cases: u64,
    near_fills: bool,
    threads: usize,
) {
    let scratch = std::env::var("ST2_SCRATCH")
        .map_or_else(|_| PathBuf::from(env!("CARGO_TARGET_TMPDIR")), PathBuf::from);
    let root = tempfile::Builder::new().prefix("st2-scan-").tempdir_in(&scratch).unwrap();

    let jobs: Vec<(PathBuf, u64)> =
        dashds.iter().flat_map(|d| (0..cases).map(move |c| (d.clone(), c))).collect();
    let failures = std::sync::Mutex::new(Vec::new());
    let totals = std::sync::Mutex::new(Vec::<Stats>::new());
    let next = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some((dashd, case)) = jobs.get(i) else {
                        break;
                    };
                    let fastprune = case % 2 == 1;
                    let dir = root.path().join(format!("{i}"));
                    std::fs::create_dir_all(&dir).unwrap();
                    let seed = if near_fills {
                        0x5732_1000
                    } else {
                        0x5732_0000
                    } + case;
                    let file = damaged(chain, seed, near_fills);
                    let boot = dir.join("bootstrap.dat");
                    std::fs::write(&boot, &file).unwrap();
                    let mut cmd = Command::new(dashd);
                    cmd.arg("-regtest")
                        .arg(format!("-datadir={}", dir.display()))
                        .arg(format!("-loadblock={}", boot.display()))
                        .args([
                            "-stopafterblockimport",
                            "-listen=0",
                            "-server=0",
                            "-printtoconsole=0",
                        ])
                        .args(["-disablewallet", "-dnsseed=0", "-fixedseeds=0", "-connect=0"])
                        // Regtest's default -checkblockindex aborts when blocks are accepted
                        // before the genesis record is seen (no active chain yet).
                        .arg("-checkblockindex=0");
                    if fastprune {
                        cmd.arg("-fastprune");
                    }
                    let status = cmd.status().unwrap();
                    let ours = dir.join("ours");
                    let stats = import_ours(chain, &file, &ours, fastprune);
                    // A stop requested before init completes exits with status 1 after a clean
                    // shutdown (seen with v24 when the import is fast).
                    let log =
                        std::fs::read_to_string(dir.join("regtest/debug.log")).unwrap_or_default();
                    let clean = log.contains("Stopping after block import")
                        && log.contains("Shutdown: done");
                    let result = if clean && (status.success() || status.code() == Some(1)) {
                        compare_dirs(&dir.join("regtest/blocks"), &ours).map(|_| ())
                    } else {
                        Err(format!("dashd exited with {status}"))
                    };
                    match result {
                        Ok(()) => {
                            totals.lock().unwrap().push(stats);
                            let _ = std::fs::remove_dir_all(&dir);
                        }
                        Err(e) => failures.lock().unwrap().push(format!(
                            "{} case {case} (fastprune={fastprune}, kept in {}): {e}",
                            dashd.display(),
                            dir.display()
                        )),
                    }
                }
            });
        }
    });
    let failures = failures.into_inner().unwrap();
    if !failures.is_empty() {
        std::mem::forget(root);
        panic!("{failures:#?}");
    }
    let totals = totals.into_inner().unwrap();
    let sum = |f: fn(&Stats) -> usize| totals.iter().map(f).sum::<usize>();
    eprintln!(
        "{} damaged files of {} blocks: blk/rev byte-identical to dashd -loadblock ({} blocks stored, {} connected, \
         {} with a shorter chain, {} undecodable records, {} unexpected-data rewinds)",
        jobs.len(),
        chain.blocks.len(),
        sum(|s| s.stored),
        sum(|s| s.connected),
        totals.iter().filter(|s| s.connected + 1 < chain.blocks.len()).count(),
        sum(|s| s.undecodable),
        sum(|s| s.unexpected),
    );
}

#[test]
fn scanner_and_writer_match_dashd_on_damaged_files() {
    let Some((data, dashds)) = environment() else {
        return;
    };
    let cases = std::env::var("ST2_SCAN_DIFF_CASES").map_or(24, |n| n.parse().unwrap());
    run_differential(&source_chain(&data, SMALL_CHAIN), &dashds, cases, false, 8);
}

/// Files of the whole dataset (about 39 MB), damaged around Core's buffer fill boundaries.
#[test]
fn scanner_and_writer_match_dashd_on_large_damaged_files() {
    let Some((data, dashds)) = environment() else {
        return;
    };
    let cases = std::env::var("ST2_SCAN_DIFF_LARGE").map_or(2, |n| n.parse().unwrap());
    run_differential(&source_chain(&data, usize::MAX), &dashds, cases, true, 4);
}

/// The version probe's classification, on fake `dashd` scripts in a private (0700) directory.
#[cfg(unix)]
mod probe {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Stdio;

    use super::*;

    /// No pause between attempts: the tests exercise the logic, not the timing.
    const NO_BACKOFF: Duration = Duration::ZERO;

    /// A fake `dashd` whose body is the shell script `body`.
    ///
    /// A child writes the file: were this process to hold the write descriptor, a concurrent
    /// test's fork would inherit it and executing the script would fail with ETXTBSY (a spawn
    /// failure the probe counts as an attempt).
    fn fake(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        let mut writer = Command::new("sh")
            .args(["-c", "umask 077; cat > \"$0\" && chmod 700 \"$0\""])
            .arg(&path)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        writer.stdin.take().unwrap().write_all(format!("#!/bin/sh\n{body}\n").as_bytes()).unwrap();
        assert!(writer.wait().unwrap().success());
        path
    }

    fn private_dir() -> tempfile::TempDir {
        // `tempfile` would otherwise create the directory with 0777 minus the umask.
        tempfile::Builder::new()
            .prefix("bl85-probe-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
            .unwrap()
    }

    fn undetermined(probe: &Probe) -> (&str, &str, &str) {
        match probe {
            Probe::Undetermined {
                status,
                stdout,
                stderr,
            } => (status, stdout, stderr),
            other => panic!("expected Undetermined, got {other:?}"),
        }
    }

    #[test]
    fn version_lines_are_classified() {
        let dir = private_dir();
        let v24 = fake(dir.path(), "v24", "echo 'Dash Core version v24.0.0-rc.3'");
        let v23 = fake(dir.path(), "v23", "echo 'Dash Core version v23.1.8'; echo Copyright");
        assert_eq!(probe_dashd(&v24, NO_BACKOFF), Probe::V24);
        assert_eq!(
            probe_dashd(&v23, NO_BACKOFF),
            Probe::NotV24("Dash Core version v23.1.8".to_owned())
        );
        assert!(verdict(&v24, Probe::V24));
        assert!(!verdict(&v23, Probe::NotV24("Dash Core version v23.1.8".to_owned())));
    }

    #[test]
    fn empty_output_is_undetermined_not_v23() {
        let dir = private_dir();
        let silent = fake(dir.path(), "silent", "exit 0");
        let probe = probe_dashd(&silent, NO_BACKOFF);
        assert_eq!(undetermined(&probe).1, "");
    }

    #[test]
    fn failed_run_is_undetermined_even_with_a_version_line() {
        let dir = private_dir();
        let failing =
            fake(dir.path(), "failing", "echo 'Dash Core version v24.0.0'; echo oops >&2; exit 1");
        let probe = probe_dashd(&failing, NO_BACKOFF);
        let (status, stdout, stderr) = undetermined(&probe);
        assert!(status.contains('1'), "{status}");
        assert!(stdout.contains("v24.0.0"), "{stdout}");
        assert_eq!(stderr, "oops\n");
    }

    #[test]
    fn output_without_a_version_line_is_undetermined() {
        let dir = private_dir();
        let odd = fake(dir.path(), "odd", "echo 'something else'");
        let probe = probe_dashd(&odd, NO_BACKOFF);
        assert_eq!(undetermined(&probe).1, "something else\n");
        // A line cut short is no answer either.
        let cut = fake(dir.path(), "cut", "echo 'Dash Core version v2'");
        assert!(matches!(probe_dashd(&cut, NO_BACKOFF), Probe::Undetermined { .. }));
    }

    #[test]
    fn missing_binary_is_undetermined() {
        let dir = private_dir();
        let probe = probe_dashd(&dir.path().join("absent"), NO_BACKOFF);
        assert!(undetermined(&probe).0.contains("failed to run"));
    }

    /// The first runs print nothing, as under load: the retries reach the real answer.
    #[test]
    fn transient_empty_output_is_retried() {
        let dir = private_dir();
        let counter = dir.path().join("runs");
        let flaky = fake(
            dir.path(),
            "flaky",
            &format!(
                "runs=\"$(dirname \"$0\")/runs\"; echo run >> \"$runs\"; \
                 if [ \"$(wc -l < \"$runs\")\" -le {} ]; then exit 0; fi; \
                 echo 'Dash Core version v24.0.0-rc.3'",
                PROBE_ATTEMPTS - 1,
            ),
        );
        assert_eq!(probe_dashd(&flaky, NO_BACKOFF), Probe::V24);
        assert_eq!(
            std::fs::read_to_string(&counter).unwrap().lines().count(),
            PROBE_ATTEMPTS as usize
        );
    }

    /// Every run gets its own existing, empty datadir: `-version` rewrites `settings.json` there,
    /// so a datadir shared between concurrent probes (the default `~/.dashcore`) makes them fail.
    #[test]
    fn each_probe_runs_in_its_own_empty_datadir() {
        let dir = private_dir();
        let strict = fake(
            dir.path(),
            "strict",
            "case \"$1\" in -datadir=*) d=\"${1#-datadir=}\" ;; *) exit 1 ;; esac; \
             [ -d \"$d\" ] && [ -z \"$(ls -A \"$d\")\" ] || exit 1; \
             echo \"$d\" >> \"$(dirname \"$0\")/datadirs\"; \
             echo 'Dash Core version v24.0.0-rc.3'",
        );
        assert_eq!(probe_dashd(&strict, NO_BACKOFF), Probe::V24);
        assert_eq!(probe_dashd(&strict, NO_BACKOFF), Probe::V24);
        let used = std::fs::read_to_string(dir.path().join("datadirs")).unwrap();
        let used: HashSet<&str> = used.lines().collect();
        assert_eq!(used.len(), 2, "{used:?}");
    }

    #[test]
    fn gives_up_after_the_attempt_limit() {
        let dir = private_dir();
        let counter = dir.path().join("runs");
        let silent = fake(dir.path(), "silent", "echo run >> \"$(dirname \"$0\")/runs\"");
        assert!(matches!(probe_dashd(&silent, NO_BACKOFF), Probe::Undetermined { .. }));
        assert_eq!(
            std::fs::read_to_string(&counter).unwrap().lines().count(),
            PROBE_ATTEMPTS as usize
        );
    }

    #[test]
    #[should_panic(expected = "could not determine the version of")]
    fn undetermined_verdict_panics() {
        let probe = Probe::Undetermined {
            status: "exit status: 1".to_owned(),
            stdout: String::new(),
            stderr: "disk quota exceeded".to_owned(),
        };
        verdict(Path::new("dashd"), probe);
    }
}
