//! `scan` end to end on a synthetic `blocks/` directory: the best chain and a stale sibling
//! are classified, an orphan is counted, outputs are deterministic, and the directory is only
//! read.

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use dash_conformance::check::{parse_rev_hex, rev_hex};
use dash_conformance::scan::{Net, ScanOptions, ScanSet, merkle_root, run_scan};
use dashcore::hashes::{Hash, sha256d};

const MAGIC: [u8; 4] = [0xbf, 0x0c, 0x6b, 0xbd];

fn compact(n: usize, out: &mut Vec<u8>) {
    assert!(n < 0xfd);
    out.push(n as u8);
}

fn tx(version: i16, ty: u16, payload: Option<Vec<u8>>) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&(version as u16 as u32 | (u32::from(ty) << 16)).to_le_bytes());
    compact(1, &mut b);
    b.extend_from_slice(&[0; 36]);
    compact(2, &mut b);
    b.extend_from_slice(&[0x51, 0x51]);
    b.extend_from_slice(&[0xff; 4]);
    compact(1, &mut b);
    b.extend_from_slice(&50u64.to_le_bytes());
    compact(1, &mut b);
    b.push(0x51);
    b.extend_from_slice(&[0; 4]);
    if let Some(p) = payload {
        compact(p.len(), &mut b);
        b.extend_from_slice(&p);
    }
    b
}

fn mnhf_payload(bit: u8) -> Vec<u8> {
    let mut p = vec![1, bit];
    p.extend_from_slice(&[9; 32]);
    p.extend_from_slice(&[0; 96]);
    p
}

fn header(prev: [u8; 32], merkle: [u8; 32], nonce: u32) -> Vec<u8> {
    let mut h = Vec::new();
    h.extend_from_slice(&1i32.to_le_bytes());
    h.extend_from_slice(&prev);
    h.extend_from_slice(&merkle);
    h.extend_from_slice(&0u32.to_le_bytes());
    h.extend_from_slice(&0u32.to_le_bytes());
    h.extend_from_slice(&nonce.to_le_bytes());
    h
}

fn hash(header: &[u8]) -> [u8; 32] {
    dashcore::consensus::deserialize::<dashcore::block::Header>(header)
        .expect("header")
        .block_hash()
        .to_byte_array()
}

fn block(prev: [u8; 32], nonce: u32, txs: &[Vec<u8>]) -> (Vec<u8>, [u8; 32]) {
    let txids: Vec<_> = txs.iter().map(|t| sha256d::Hash::hash(t).to_byte_array()).collect();
    let h = header(prev, merkle_root(&txids), nonce);
    let id = hash(&h);
    let mut b = h;
    compact(txs.len(), &mut b);
    for t in txs {
        b.extend_from_slice(t);
    }
    (b, id)
}

fn record(out: &mut Vec<u8>, block: &[u8]) {
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&(block.len() as u32).to_le_bytes());
    out.extend_from_slice(block);
}

/// genesis <- 1 <- 2 <- 3 <- 4, a stale sibling of 3 and an orphan; returns the genesis and
/// tip hashes.
fn write_chain(dir: &Path) -> ([u8; 32], [u8; 32]) {
    let cb = || tx(1, 0, None);
    let (g, gh) = block([0; 32], 0, &[cb()]);
    let (b1, h1) = block(gh, 1, &[cb()]);
    let (b2, h2) = block(h1, 2, &[cb(), tx(2, 0x2a, None)]);
    let (b3, h3) = block(h2, 3, &[cb()]);
    let (b4, h4) = block(h3, 4, &[cb(), tx(3, 7, Some(mnhf_payload(10)))]);
    // a stale sibling of 3 with a version-4 typed tx, and a block whose parent is unknown
    let (stale, _) = block(h2, 33, &[cb(), tx(4, 7, Some(vec![0xaa]))]);
    let (orphan, _) = block([0xee; 32], 99, &[cb()]);
    let mut file = Vec::new();
    for b in [&b2, &g, &stale, &b1, &b4, &b3, &orphan] {
        record(&mut file, b);
    }
    file.extend_from_slice(&[0; 128]);
    fs::write(dir.join("blk00000.dat"), file).unwrap();
    (gh, h4)
}

fn options<'a>(dir: &Path, out: &Path, genesis: [u8; 32]) -> ScanOptions<'a> {
    ScanOptions {
        net: Net::Mainnet,
        blocks_dir: dir.to_path_buf(),
        scans: ScanSet::parse("abdef").unwrap(),
        magic: MAGIC,
        genesis,
        from: 0,
        to: None,
        anchor: None,
        oracle: None,
        out: out.to_path_buf(),
        code_id: "test".into(),
    }
}

fn read_outputs(out: &Path) -> Vec<(String, Vec<u8>)> {
    let mut names: Vec<PathBuf> = fs::read_dir(out).unwrap().map(|e| e.unwrap().path()).collect();
    names.sort();
    names
        .into_iter()
        .map(|p| (p.file_name().unwrap().to_string_lossy().into_owned(), fs::read(&p).unwrap()))
        .collect()
}

#[test]
fn classifies_the_best_chain_and_the_stale_block_apart() {
    let dir = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let (genesis, tip) = write_chain(dir.path());
    let mut opts = options(dir.path(), out.path(), genesis);
    opts.anchor = Some(tip);
    let res = run_scan(&opts).unwrap();
    let s = &res.summary;
    assert_eq!(s["tip"]["height"], 4);
    assert_eq!(s["hit_counts"]["b.pre_dip3"], 1, "{s}");
    assert_eq!(s["hit_counts"]["e.signal"], 1, "{s}");
    assert_eq!(s["stale_hit_counts"]["a.version_gt3_typed"], 1, "{s}");
    assert_eq!(s["stale_hit_counts"]["a.framing_differs"], 1, "{s}");
    assert!(s["hit_counts"].get("a.version_gt3_typed").is_none(), "best chain has no such tx");
    assert_eq!(s["inputs"]["unlinked_records"], 1);
    assert_eq!(s["counters"]["blocks"], 5);
    assert_eq!(s["counters"]["stale.blocks"], 1);
    assert!(res.incomplete.is_empty(), "{:?}", res.incomplete);
    assert!(out.path().join("scan-b.ndjson").exists());
    // the MNHF signal in block 4 carries the median time past of block 3 (every header's time is 0)
    let e = fs::read_to_string(out.path().join("scan-e.ndjson")).unwrap();
    assert!(e.contains("\"mtp_prev\":0"), "{e}");
}

#[test]
fn an_unanchored_scan_with_unlinked_records_is_incomplete() {
    let dir = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let (genesis, _) = write_chain(dir.path());
    let res = run_scan(&options(dir.path(), out.path(), genesis)).unwrap();
    assert_eq!(res.incomplete.len(), 1, "{:?}", res.incomplete);
    assert!(res.incomplete[0].contains("no anchor"), "{:?}", res.incomplete);
}

#[test]
fn ranges_beyond_the_tip_and_an_output_inside_the_blocks_directory_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let (genesis, tip) = write_chain(dir.path());

    let mut from_above = options(dir.path(), out.path(), genesis);
    from_above.anchor = Some(tip);
    from_above.from = 5;
    let err = run_scan(&from_above).unwrap_err().to_string();
    assert!(err.contains("above the chain tip"), "{err}");

    let mut to_above = options(dir.path(), out.path(), genesis);
    to_above.anchor = Some(tip);
    to_above.to = Some(9);
    let res = run_scan(&to_above).unwrap();
    assert_eq!(res.incomplete.len(), 1, "{:?}", res.incomplete);
    assert!(res.incomplete[0].contains("--to 9 is above the chain tip 4"));

    let inside = dir.path().join("scan-out");
    let err = run_scan(&options(dir.path(), &inside, genesis)).unwrap_err().to_string();
    assert!(err.contains("which is only read"), "{err}");
    assert!(!inside.exists());
}

#[test]
fn the_output_guard_resolves_symlinks_and_covers_the_whole_datadir() {
    let datadir = tempfile::tempdir().unwrap();
    let blocks = datadir.path().join("blocks");
    fs::create_dir(&blocks).unwrap();
    let (genesis, _) = write_chain(&blocks);
    let elsewhere = tempfile::tempdir().unwrap();
    let listing = |d: &Path| {
        let mut v: Vec<_> = fs::read_dir(d).unwrap().map(|e| e.unwrap().file_name()).collect();
        v.sort();
        v
    };
    let before = (listing(&blocks), listing(datadir.path()));
    let refused = |out: &Path| {
        let err = run_scan(&options(&blocks, out, genesis)).unwrap_err().to_string();
        assert!(err.contains("only read") || err.contains("dangling"), "{}: {err}", out.display());
    };

    // an existing symlink to the blocks directory, and a new directory under it
    let to_blocks = elsewhere.path().join("to-blocks");
    symlink(&blocks, &to_blocks).unwrap();
    refused(&to_blocks);
    refused(&to_blocks.join("new-output"));
    // the datadir around the blocks directory, directly and through a symlink
    refused(&datadir.path().join("out"));
    let to_datadir = elsewhere.path().join("to-datadir");
    symlink(datadir.path(), &to_datadir).unwrap();
    refused(&to_datadir.join("a").join("b"));
    // a dangling symlink whose target would be created inside the blocks directory
    let dangling = elsewhere.path().join("dangling");
    symlink(blocks.join("not-yet"), &dangling).unwrap();
    refused(&dangling);

    assert_eq!(before, (listing(&blocks), listing(datadir.path())));
    // an unrelated output still works, and through a symlink to an unrelated place
    let fine = elsewhere.path().join("fine");
    run_scan(&options(&blocks, &fine, genesis)).unwrap();
    let via_link = elsewhere.path().join("via-link");
    symlink(elsewhere.path().join("target"), &via_link).unwrap();
    fs::create_dir(elsewhere.path().join("target")).unwrap();
    run_scan(&options(&blocks, &via_link.join("o"), genesis)).unwrap();
}

#[test]
fn the_output_guard_protects_the_datadir_when_blocks_is_itself_a_symlink() {
    let elsewhere = tempfile::tempdir().unwrap();
    let real_blocks = elsewhere.path().join("real").join("blocks");
    fs::create_dir_all(&real_blocks).unwrap();
    let (genesis, _) = write_chain(&real_blocks);
    let listing = |d: &Path| {
        let mut v: Vec<_> = fs::read_dir(d).unwrap().map(|e| e.unwrap().file_name()).collect();
        v.sort();
        v
    };

    // <datadir>/blocks and <datadir>/testnet3/blocks, each a symlink to the real directory
    let datadir = tempfile::tempdir().unwrap();
    let chain_dir = datadir.path().join("testnet3");
    fs::create_dir(&chain_dir).unwrap();
    symlink(&real_blocks, datadir.path().join("blocks")).unwrap();
    symlink(&real_blocks, chain_dir.join("blocks")).unwrap();
    let before = (listing(datadir.path()), listing(&chain_dir), listing(&real_blocks));

    let refused = |blocks: &Path, out: &Path| {
        let err = run_scan(&options(blocks, out, genesis)).unwrap_err().to_string();
        assert!(err.contains("only read"), "{}: {err}", out.display());
        assert!(!out.exists(), "{}", out.display());
    };
    refused(&datadir.path().join("blocks"), &datadir.path().join("out"));
    // the same through a symlink to the datadir, and with a trailing slash
    let to_datadir = elsewhere.path().join("to-datadir");
    symlink(datadir.path(), &to_datadir).unwrap();
    refused(&to_datadir.join("blocks"), &datadir.path().join("out2"));
    refused(&datadir.path().join("blocks"), &to_datadir.join("out3"));
    refused(&chain_dir.join("blocks"), &chain_dir.join("out"));
    refused(&chain_dir.join("blocks"), &datadir.path().join("out4"));

    assert_eq!(before, (listing(datadir.path()), listing(&chain_dir), listing(&real_blocks)));
    // an unrelated output still works
    run_scan(&options(&datadir.path().join("blocks"), &elsewhere.path().join("fine"), genesis))
        .unwrap();
}

#[test]
fn outputs_are_deterministic() {
    let dir = tempfile::tempdir().unwrap();
    let (genesis, _) = write_chain(dir.path());
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    run_scan(&options(dir.path(), first.path(), genesis)).unwrap();
    run_scan(&options(dir.path(), second.path(), genesis)).unwrap();
    assert_eq!(read_outputs(first.path()), read_outputs(second.path()));
}

#[test]
fn the_blocks_directory_is_only_read() {
    let dir = tempfile::tempdir().unwrap();
    let (genesis, _) = write_chain(dir.path());
    let file = dir.path().join("blk00000.dat");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o444)).unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o555)).unwrap();
    let snapshot = |p: &Path| {
        let m = fs::metadata(p).unwrap();
        (m.len(), m.modified().unwrap())
    };
    let before = (fs::read(&file).unwrap(), snapshot(&file), snapshot(dir.path()));
    let listing = |d: &Path| {
        let mut v: Vec<_> = fs::read_dir(d).unwrap().map(|e| e.unwrap().file_name()).collect();
        v.sort();
        v
    };
    let names = listing(dir.path());
    let out = tempfile::tempdir().unwrap();
    run_scan(&options(dir.path(), out.path(), genesis)).unwrap();
    let after = (fs::read(&file).unwrap(), snapshot(&file), snapshot(dir.path()));
    assert_eq!(before, after);
    assert_eq!(names, listing(dir.path()));
    // let tempfile clean up
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
}

/// Under `strace`, the scan opens nothing in the blocks directory for writing and makes no
/// write-class call on it. Skipped where `strace` is absent.
#[test]
fn no_write_syscall_touches_the_blocks_directory() {
    let strace = std::process::Command::new("strace").arg("-V").output();
    if strace.is_err() {
        eprintln!("strace not installed; skipped");
        return;
    }
    // The test binary re-runs itself as the traced process.
    let dir = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let (genesis, _) = write_chain(dir.path());
    let log = out.path().join("strace.log");
    let exe = std::env::current_exe().unwrap();
    let status = std::process::Command::new("strace")
        .args(["-f", "-qq", "-e", "trace=file,desc", "-o"])
        .arg(&log)
        .arg(&exe)
        .args(["--exact", "traced_child", "--nocapture", "--ignored"])
        .env("O6_SCAN_DIR", dir.path())
        .env("O6_SCAN_OUT", out.path().join("scan-out"))
        .env("O6_SCAN_GENESIS", rev_hex(&genesis))
        .status()
        .unwrap();
    assert!(status.success());
    let text = fs::read_to_string(&log).unwrap();
    let dir_s = dir.path().to_string_lossy().to_string();
    let touching: Vec<&str> = text.lines().filter(|l| l.contains(&dir_s)).collect();
    assert!(!touching.is_empty(), "the trace saw nothing of the directory");
    for l in touching {
        let writes =
            ["O_WRONLY", "O_RDWR", "O_CREAT", "O_TRUNC", "unlink", "rename", "mkdir", "chmod"];
        assert!(!writes.iter().any(|w| l.contains(w)), "write-class call on the directory: {l}");
    }
}

#[test]
#[ignore = "run by no_write_syscall_touches_the_blocks_directory under strace"]
fn traced_child() {
    let dir = PathBuf::from(std::env::var("O6_SCAN_DIR").unwrap());
    let out = PathBuf::from(std::env::var("O6_SCAN_OUT").unwrap());
    let genesis = parse_rev_hex(&std::env::var("O6_SCAN_GENESIS").unwrap()).unwrap();
    run_scan(&options(&dir, &out, genesis)).unwrap();
}
