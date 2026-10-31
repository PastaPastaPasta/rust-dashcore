//! Segment snapshot bundles (O9b S2): `segment bundle` and `segment validate` over a synthetic
//! regtest chain of 110 blocks (block 102 spends block 1's coinbase), its log from genesis as
//! the golden, and the snapshot a run to 100 saves (CH3a D165).
//!
//! - A bundle validates, and `digest l2 --start-utxo` runs its window 101..=110 from it.
//! - Each check refuses on its own: a snapshot whose sidecar records another scan, a golden whose
//!   MuHash at the base differs, a changed byte in the bundle, a file the checksums do not list,
//!   checksums listing a file twice or not every file, a snapshot or manifest that is not the
//!   sidecar's (even with its checksum rewritten), and an output directory that exists. A refused
//!   `bundle` leaves no directory.
//! - The evo section (S6a-2, manifest v2): Core's `EvoSnapshot` at the base bundles, opens and
//!   validates with its sha256 and `GetEvoSnapshotHash` recorded; one at another block or height,
//!   with bytes after it or that does not decode is refused; a changed section, a manifest that is
//!   not the section's and an evo section in a v1 manifest are refused; a v1 bundle still opens.
//! - Core's own bytes: rc.4's `populated` vector scans to the hash Core's test pins, with its
//!   three masternodes, under mainnet's rules only; with its addresses moved to unroutable ones
//!   off port 9999, regtest's rules accept it and testnet's refuse it.
//! - Checksums naming a file outside the bundle (an absolute path, a `../` or a subdirectory) are
//!   refused before any is read, and so is a bundle file that is a symlink.

// Shared with the digest tests; this file uses the chain helpers only.
#[allow(dead_code)]
mod digest_common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use dash_conformance::digest::blkdir::BlockDir;
use dash_conformance::digest::l2::{self, Options, Source};
use dash_conformance::digest::utxo::sidecar_path;
use dash_conformance::segment::bundle::{
    self, EVO, FORMAT, FORMAT_V1, MANIFEST, SIDECAR, SUMS, Scan, UTXO,
};
use dash_consensus::primitives::{Tx, core_serialize};
use dash_dml::MnList;
use dash_dml::netinfo::{CoreNetInfoValidity, NetInfoConfig, NetInfoRules};
use dash_dml::snapshot::EvoSnapshot;
use dash_params::{ChainParamsBuilder, ChainSetup, Network, Release};
use dash_statedigest::{Chain, Hash256};
use dashd_chainstate::chain::genesis_block;
use digest_common::{coinbase_sig, decode, mine, tempdir};

const WINDOW: u32 = 110;
const BASE: u32 = 100;
const SPEND_AT: u32 = 102;
const COIN: i64 = 100_000_000;

fn regtest() -> ChainSetup {
    ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap()
}

fn options() -> Options {
    Options {
        assume_valid: false,
        par: 1,
        coins_cache: 256 << 20,
        reverse_to: None,
        golden: None,
        sha256: false,
        headers: None,
        save_utxo: None,
        start_utxo: None,
    }
}

/// A version-1 transaction with one output of `COIN` to `OP_TRUE`.
fn tx(prevout: ([u8; 32], u32), sig: &[u8]) -> Arc<Tx> {
    let mut raw = 1i32.to_le_bytes().to_vec();
    raw.push(1);
    raw.extend(prevout.0);
    raw.extend(prevout.1.to_le_bytes());
    raw.push(sig.len() as u8);
    raw.extend(sig);
    raw.extend(u32::MAX.to_le_bytes());
    raw.push(1);
    raw.extend(COIN.to_le_bytes());
    raw.extend([1, 0x51]);
    raw.extend(0u32.to_le_bytes());
    decode(&raw)
}

/// Regtest's chain to `WINDOW` in `<dir>/blocks/blk00000.dat`.
fn chain(p: &ChainSetup) -> tempfile::TempDir {
    use std::io::Write as _;
    let dir = tempdir();
    let blocks = dir.path().join("blocks");
    std::fs::create_dir(&blocks).unwrap();
    let mut file =
        std::io::BufWriter::new(std::fs::File::create(blocks.join("blk00000.dat")).unwrap());
    let mut prev = genesis_block(p);
    let mut first_coinbase = None;
    for h in 0..=WINDOW {
        if h > 0 {
            let mut txs = vec![tx(([0; 32], u32::MAX), &coinbase_sig(h))];
            if h == SPEND_AT {
                txs.push(tx((first_coinbase.unwrap(), 0), &[]));
            }
            let b = mine(&prev, txs);
            if h == 1 {
                first_coinbase = Some(*b.txs()[0].txid().as_bytes());
            }
            prev = b;
        }
        let bytes = core_serialize(&*prev);
        file.write_all(&p.message_start).unwrap();
        file.write_all(&(bytes.len() as u32).to_le_bytes()).unwrap();
        file.write_all(&bytes).unwrap();
    }
    file.into_inner().unwrap().sync_all().unwrap();
    dir
}

fn run(dir: &BlockDir, p: &ChainSetup, opts: &Options, out: &Path, name: &str, to: u32) {
    let source = Source::Dir {
        dir,
        params: Arc::new(p.clone()),
        chain: Chain::Regtest,
        to,
    };
    let log = out.join(format!("{name}.jsonl"));
    let manifest = out.join(format!("{name}.manifest.json"));
    l2::run(source, opts, &log, &manifest, &out.join(format!("{name}-work"))).unwrap();
}

/// The fixture: the chain, its blocks directory, the golden (the log from genesis) and the
/// snapshot at `BASE` with its sidecar, in `out`.
struct Fixture {
    _src: tempfile::TempDir,
    out: tempfile::TempDir,
    dir: BlockDir,
    p: ChainSetup,
    golden: PathBuf,
    snapshot: PathBuf,
}

fn fixture() -> Fixture {
    let p = regtest();
    let src = chain(&p);
    let dir =
        BlockDir::open(&src.path().join("blocks"), p.message_start, genesis_block(&p).hash(), None)
            .unwrap();
    let out = tempdir();
    run(&dir, &p, &options(), out.path(), "g", WINDOW);
    let golden = out.path().join("g.jsonl");
    let snapshot = out.path().join("s.dat");
    let mut opts = options();
    opts.golden = Some(golden.clone());
    opts.save_utxo = Some(snapshot.clone());
    run(&dir, &p, &opts, out.path(), "save", BASE);
    Fixture {
        _src: src,
        out,
        dir,
        p,
        golden,
        snapshot,
    }
}

fn make(f: &Fixture, name: &str) -> anyhow::Result<PathBuf> {
    let at = f.out.path().join(name);
    bundle::make(&f.snapshot, None, &f.golden, "test transition", &at).map(|m| m.dir)
}

/// [`make`] with `evo`'s bytes as the evo section.
fn make_evo(f: &Fixture, name: &str, evo: &[u8]) -> anyhow::Result<PathBuf> {
    let file = f.out.path().join(format!("{name}.evo"));
    std::fs::write(&file, evo).unwrap();
    let at = f.out.path().join(name);
    bundle::make(&f.snapshot, Some(&file), &f.golden, "test transition", &at).map(|m| m.dir)
}

/// The base block's hash, from a bundle without an evo section.
fn base_hash(f: &Fixture) -> dash_primitives::BlockHash {
    let b = make(f, "for-base").unwrap();
    dash_primitives::BlockHash::from(bundle::open(&b).unwrap().scan.hash.to_wire_bytes())
}

/// Core's `EvoSnapshot` with an empty list at `block` and `height`.
fn evo_snapshot(block: dash_primitives::BlockHash, height: i32) -> EvoSnapshot {
    EvoSnapshot {
        base_block_hash: block,
        mn_list: MnList::new(block, height, 0),
        ..EvoSnapshot::default()
    }
}

fn encoded(snapshot: &EvoSnapshot) -> Vec<u8> {
    let mut bytes = Vec::new();
    snapshot.encode(&mut bytes);
    bytes
}

/// `GetEvoSnapshotHash` in display order, as the manifest records it.
fn evo_hash(snapshot: &EvoSnapshot) -> String {
    let regtest = regtest();
    let main = ChainParamsBuilder::new(Network::Main, Release::V24_0).build().unwrap();
    let net = CoreNetInfoValidity {
        rules: NetInfoRules::new(&regtest, &main, NetInfoConfig::default()),
    };
    let hash = snapshot.hash(&net).unwrap();
    hash.as_bytes().iter().rev().map(|b| format!("{b:02x}")).collect()
}

fn manifest(b: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(b.join(MANIFEST)).unwrap()).unwrap()
}

/// Replaces `file` in bundle `b` by `edit` of it, with its checksum rewritten to match.
fn rewrite(b: &Path, file: &str, edit: &dyn Fn(Vec<u8>) -> Vec<u8>) {
    use dash_conformance::digest::store::sha256_hex as digest;
    let old = std::fs::read(b.join(file)).unwrap();
    let new = edit(old.clone());
    assert_ne!(new, old);
    std::fs::write(b.join(file), &new).unwrap();
    let sums = std::fs::read_to_string(b.join(SUMS)).unwrap();
    std::fs::write(b.join(SUMS), sums.replace(&digest(&old), &digest(&new))).unwrap();
}

fn text_edit(from: impl Into<String>, to: impl Into<String>) -> impl Fn(Vec<u8>) -> Vec<u8> {
    let (from, to) = (from.into(), to.into());
    move |bytes: Vec<u8>| String::from_utf8(bytes).unwrap().replacen(&from, &to, 1).into_bytes()
}

fn error(r: anyhow::Result<impl std::fmt::Debug>) -> String {
    format!("{:#}", r.unwrap_err())
}

#[test]
fn a_bundle_validates_and_starts_its_window() {
    let f = fixture();
    let b = make(&f, "b").unwrap();
    let mut names: Vec<_> = std::fs::read_dir(&b)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, [MANIFEST, SUMS, UTXO, SIDECAR]);
    let v = bundle::validate(&b, &f.golden).unwrap();
    assert_eq!((v.chain.as_str(), v.transition.as_str()), ("regtest", "test transition"));
    assert_eq!((v.scan.height, v.scan.coins), (BASE, BASE as usize));
    let manifest = manifest(&b);
    assert_eq!(manifest["format"], FORMAT);
    assert_eq!(manifest["evo"], serde_json::Value::Null);
    assert_eq!(manifest["base"]["height"], BASE);
    assert!(v.evo.is_none());

    // The window from the bundle's snapshot.
    let mut opts = options();
    opts.golden = Some(f.golden.clone());
    opts.start_utxo = Some(v.utxo.clone());
    opts.reverse_to = Some(BASE);
    run(&f.dir, &f.p, &opts, f.out.path(), "seg", WINDOW);
}

#[test]
fn an_existing_output_is_refused_and_a_failed_bundle_leaves_nothing() {
    let f = fixture();
    make(&f, "b").unwrap();
    assert!(error(make(&f, "b")).contains("creating"));

    // A sidecar recording another scan: refused before the directory is created.
    let side = sidecar_path(&f.snapshot);
    let text = std::fs::read_to_string(&side).unwrap();
    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
    let txouts = json["scan"]["txouts"].as_u64().unwrap();
    let edited =
        text.replace(&format!("\"txouts\": {txouts}"), &format!("\"txouts\": {}", txouts + 1));
    assert_ne!(edited, text);
    std::fs::write(&side, edited).unwrap();
    let e = error(make(&f, "c"));
    assert!(e.contains("its sidecar records"), "{e}");
    assert!(!f.out.path().join("c").exists());
    assert!(!f.out.path().join(".c.partial").exists());
}

/// A killed run's partial directory is refused with what to do, and left as it is: removing it
/// would also let a concurrent maker's directory be taken over.
#[test]
fn a_left_partial_directory_is_refused_and_kept() {
    let f = fixture();
    let partial = f.out.path().join(".c.partial");
    std::fs::create_dir(&partial).unwrap();
    std::fs::write(partial.join("left"), b"x").unwrap();
    let e = error(make(&f, "c"));
    assert!(e.contains("a previous run left it (killed?); remove it to rebuild"), "{e}");
    assert_eq!(std::fs::read(partial.join("left")).unwrap(), b"x");
    assert!(!f.out.path().join("c").exists());
}

#[test]
fn a_golden_whose_muhash_differs_at_the_base_is_refused() {
    let f = fixture();
    let b = make(&f, "b").unwrap();
    let text = std::fs::read_to_string(&f.golden).unwrap();
    let line = text
        .lines()
        .find(|l| l.contains(r#""k":"C""#) && l.contains(&format!(r#""h":{BASE},"#)))
        .expect("golden(BASE)");
    let at = line.find(r#""muhash":""#).unwrap() + 10;
    let old = &line[at..at + 64];
    let flipped = if old.starts_with('0') {
        "1"
    } else {
        "0"
    };
    let new = line.replacen(old, &format!("{flipped}{}", &old[1..]), 1);
    let bad = f.out.path().join("bad.jsonl");
    std::fs::write(&bad, text.replacen(line, &new, 1)).unwrap();
    let e = error(bundle::validate(&b, &bad));
    assert!(e.contains(&format!("the snapshot at {BASE} gives muhash")), "{e}");
    assert!(
        error(bundle::make(&f.snapshot, None, &bad, "t", &f.out.path().join("c")))
            .contains("muhash")
    );
}

#[test]
fn a_changed_bundle_is_refused() {
    let f = fixture();
    let fresh = |name: &str| make(&f, name).unwrap();

    let b = fresh("byte");
    let utxo = b.join(UTXO);
    let mut bytes = std::fs::read(&utxo).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    std::fs::write(&utxo, bytes).unwrap();
    let e = error(bundle::open(&b));
    assert!(e.contains(&format!("{UTXO} has sha256")), "{e}");

    let b = fresh("extra");
    std::fs::write(b.join("notes.txt"), b"x").unwrap();
    assert!(error(bundle::open(&b)).contains("holds"));

    // Files edited with their checksums rewritten: each check against the sidecar still refuses.
    for (name, file, edit, want) in [
        ("chain", MANIFEST, text_edit("\"regtest\"", "\"test\""), "the manifest's chain"),
        ("coins", MANIFEST, text_edit("\"coins\": 100", "\"coins\": 99"), "utxo section"),
        // An evo section the files do not hold.
        ("evo", MANIFEST, text_edit("\"evo\": null", "\"evo\": {}"), "a bundle has"),
        ("base", MANIFEST, text_edit("\"height\": 100", "\"height\": 99"), "the manifest's base"),
    ] {
        let b = fresh(name);
        rewrite(&b, file, &edit);
        let e = error(bundle::open(&b));
        assert!(e.contains(want), "{name}: {e}");
    }
    // Another snapshot under the bundle's name.
    let b = fresh("utxo");
    rewrite(&b, UTXO, &|mut bytes| {
        bytes.push(0);
        bytes
    });
    let e = error(bundle::open(&b));
    assert!(e.contains("its sidecar records"), "{e}");

    // Checksums listing a file twice, or not every file.
    let b = fresh("twice");
    let sums = std::fs::read_to_string(b.join(SUMS)).unwrap();
    let first = sums.lines().next().unwrap().to_owned();
    std::fs::write(b.join(SUMS), format!("{sums}{first}\n")).unwrap();
    assert!(error(bundle::open(&b)).contains("twice"));
    let b = fresh("subset");
    let sums = std::fs::read_to_string(b.join(SUMS)).unwrap();
    let kept: String =
        sums.lines().filter(|l| !l.ends_with(SIDECAR)).map(|l| format!("{l}\n")).collect();
    std::fs::write(b.join(SUMS), kept).unwrap();
    assert!(error(bundle::open(&b)).contains("lists"));

    let b = fresh("missing");
    std::fs::remove_file(b.join(SIDECAR)).unwrap();
    assert!(error(bundle::open(&b)).contains(SIDECAR));
}

/// Checksums naming a file outside the bundle are refused before it is read, even with the file's
/// right sha256 (a device such as `/dev/zero` would never finish hashing); so is a symlink.
#[test]
fn a_listed_name_outside_the_bundle_is_refused() {
    use dash_conformance::digest::store::sha256_hex as digest;
    let f = fixture();
    let fresh = |name: &str| make(&f, name).unwrap();
    let elsewhere = fresh("elsewhere");
    let outside = elsewhere.join(SIDECAR);
    let hex = digest(&std::fs::read(&outside).unwrap());
    for (name, listed) in [
        ("absolute", outside.display().to_string()),
        ("traversal", format!("../elsewhere/{SIDECAR}")),
        ("subdirectory", format!("sub/{SIDECAR}")),
    ] {
        let b = fresh(name);
        std::fs::create_dir(b.join("sub")).unwrap();
        std::fs::copy(&outside, b.join("sub").join(SIDECAR)).unwrap();
        let sums = std::fs::read_to_string(b.join(SUMS)).unwrap();
        std::fs::write(b.join(SUMS), format!("{hex}  {listed}\n{sums}")).unwrap();
        let e = error(bundle::open(&b));
        assert!(e.contains(&format!("lists {listed:?}, not a bundle file")), "{name}: {e}");
    }
    // A symlink with the target's sha256, under a bundle file's name.
    let b = fresh("symlink");
    std::fs::remove_file(b.join(SIDECAR)).unwrap();
    std::os::unix::fs::symlink(&outside, b.join(SIDECAR)).unwrap();
    let e = error(bundle::open(&b));
    assert!(e.contains(&format!("{SIDECAR} is not a regular file")), "{e}");
}

/// Core's `populated` vector (rc.4 `SyntheticSnapshot()`, `dash-dml/tests/data/evo_snapshot`),
/// with its list at 500 on block 2a00…00.
fn core_populated() -> (Vec<u8>, Scan) {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../dash-dml/tests/data/evo_snapshot/vectors.txt"
    ))
    .unwrap();
    let line = text.lines().find(|l| l.starts_with("populated ")).unwrap();
    let mut block = [0u8; 32];
    block[0] = 0x2a;
    let scan = Scan {
        height: 500,
        hash: Hash256::from_wire_bytes(block),
        coins: 0,
        muhash: Hash256::from_wire_bytes([0; 32]),
        txouts: 0,
        amount: 0,
    };
    let hex = line.split(' ').nth(2).unwrap();
    let bytes = (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap());
    (bytes.collect(), scan)
}

/// The bundle's evo scan on Core's own bytes: the hash Core's test prints, the masternode count,
/// and each chain's network-info rules.
#[test]
fn core_populated_evo_snapshot_scans_to_core_hash() {
    use dash_conformance::digest::store::sha256_hex as digest;
    let (bytes, scan) = core_populated();
    let dir = tempdir();
    let path = dir.path().join(EVO);
    std::fs::write(&path, &bytes).unwrap();
    let evo = bundle::scan_evo(&path, "main", &scan).unwrap();
    // Core: src/test/evo_snapshot_tests.cpp:317-320 at v24.0.0-rc.4,
    // GetEvoSnapshotHash(SyntheticSnapshot()).ToString().
    assert_eq!(
        evo.hash.to_hex(),
        "5b5d496a66d93775a6a6d1badf2ea2d5bbb25b58031fe0dd2c3c670674c6908d"
    );
    assert_eq!((evo.version, evo.masternodes), (3, 3));
    assert_eq!(evo.sha256, digest(&bytes));
    // Made under mainnet's rules: port 9999 is refused off mainnet.
    for chain in ["regtest", "test"] {
        let e = error(bundle::scan_evo(&path, chain, &scan));
        assert!(
            e.contains("not an evo snapshot: invalid canonical MN network info"),
            "{chain}: {e}"
        );
    }
    let mut early = scan.clone();
    early.height = 499;
    let e = error(bundle::scan_evo(&path, "main", &early));
    assert!(e.contains("the evo section's list is at height 500, the bundle's base at 499"), "{e}");

    // Its base list with the two IPv4 services moved to 10.0.0.x:19999, without the history
    // (whose diffs hash the old list): regtest's rules accept an unroutable address, testnet's
    // refuse it, and mainnet's refuse a port other than 9999.
    let main = ChainParamsBuilder::new(Network::Main, Release::V24_0).build().unwrap();
    let net = CoreNetInfoValidity {
        rules: NetInfoRules::new(&main, &main, NetInfoConfig::default()),
    };
    let core = EvoSnapshot::decode(&mut bytes.as_slice(), &net).unwrap();
    let mut list = core.mn_list.clone();
    let mut services = 0u8;
    for dmn in core.mn_list.iter() {
        let dash_primitives::NetInfo::Legacy(v1) = &*dmn.cold.net_info else {
            continue;
        };
        services += 1;
        let mut v1 = v1.clone();
        v1.0.addr.0[12..].copy_from_slice(&[10, 0, 0, services]);
        v1.0.port = 19999;
        let mut cold = (*dmn.cold).clone();
        cold.net_info = Arc::new(dash_primitives::NetInfo::Legacy(v1));
        list.update(&dmn.pro_tx_hash, dmn.hot, Arc::new(cold)).unwrap();
    }
    assert_eq!(services, 2);
    let moved = encoded(&EvoSnapshot {
        base_block_hash: core.base_block_hash,
        mn_list: list,
        ..EvoSnapshot::default()
    });
    std::fs::write(&path, &moved).unwrap();
    let evo = bundle::scan_evo(&path, "regtest", &scan).unwrap();
    assert_eq!(evo.masternodes, 3);
    for chain in ["test", "main"] {
        let e = error(bundle::scan_evo(&path, chain, &scan));
        assert!(
            e.contains("not an evo snapshot: invalid canonical MN network info"),
            "{chain}: {e}"
        );
    }
}

#[test]
fn an_evo_section_bundles_and_validates() {
    let f = fixture();
    let snapshot = evo_snapshot(base_hash(&f), BASE as i32);
    let bytes = encoded(&snapshot);
    let b = make_evo(&f, "b", &bytes).unwrap();
    let mut names: Vec<_> = std::fs::read_dir(&b)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, [MANIFEST, SUMS, EVO, UTXO, SIDECAR]);
    assert_eq!(std::fs::read(b.join(EVO)).unwrap(), bytes);
    let evo = &manifest(&b)["evo"];
    assert_eq!(
        *evo,
        serde_json::json!({
            "file": EVO,
            "sha256": dash_conformance::digest::store::sha256_hex(&bytes),
            "version": 3,
            "evo_hash": evo_hash(&snapshot),
            "masternodes": 0,
        })
    );
    let v = bundle::validate(&b, &f.golden).unwrap();
    assert_eq!(v.evo.as_ref().map(|(path, _)| path.clone()), Some(b.join(EVO)));
    assert_eq!(v.scan.height, BASE);
}

/// Each refusal leaves no bundle and no partial directory.
#[test]
fn an_evo_section_at_another_base_or_not_a_snapshot_is_refused() {
    let f = fixture();
    let base = base_hash(&f);
    let good = encoded(&evo_snapshot(base, BASE as i32));
    let mut other = *base.as_bytes();
    other[0] ^= 1;
    let mut trailing = good.clone();
    trailing.push(0);
    let mut version = good.clone();
    version[0] = 4;
    for (name, bytes, want) in [
        (
            "block",
            encoded(&evo_snapshot(dash_primitives::BlockHash::from(other), BASE as i32)),
            "the evo section's base is",
        ),
        (
            "height",
            encoded(&evo_snapshot(base, BASE as i32 - 1)),
            "the evo section's list is at height 99",
        ),
        ("trailing", trailing, "1 bytes after the evo snapshot"),
        ("version", version, "not an evo snapshot: unsupported evo snapshot version"),
        ("empty", Vec::new(), "not an evo snapshot"),
    ] {
        let e = error(make_evo(&f, name, &bytes));
        assert!(e.contains(want), "{name}: {e}");
        assert!(!f.out.path().join(name).exists(), "{name}");
        assert!(!f.out.path().join(format!(".{name}.partial")).exists(), "{name}");
    }
}

#[test]
fn a_changed_evo_section_is_refused() {
    let f = fixture();
    let snapshot = evo_snapshot(base_hash(&f), BASE as i32);
    let bytes = encoded(&snapshot);
    let fresh = |name: &str| make_evo(&f, name, &bytes).unwrap();

    let b = fresh("byte");
    let mut changed = bytes.clone();
    *changed.last_mut().unwrap() ^= 1;
    std::fs::write(b.join(EVO), &changed).unwrap();
    let e = error(bundle::open(&b));
    assert!(e.contains(&format!("{EVO} has sha256")), "{e}");

    // Another section with its checksum rewritten: not the manifest's.
    let b = fresh("other");
    rewrite(&b, EVO, &|mut bytes| {
        bytes.push(0);
        bytes
    });
    let e = error(bundle::open(&b));
    assert!(e.contains("the manifest's evo section records"), "{e}");

    // The manifest's record edited with its checksum rewritten: open checks only the file, and
    // validate decodes it again.
    let b = fresh("record");
    let hash = evo_hash(&snapshot);
    let flipped = format!(
        "{}{}",
        if hash.starts_with('0') {
            '1'
        } else {
            '0'
        },
        &hash[1..]
    );
    rewrite(&b, MANIFEST, &text_edit(hash, flipped));
    bundle::open(&b).unwrap();
    let e = error(bundle::validate(&b, &f.golden));
    assert!(e.contains("the manifest records"), "{e}");

    let b = fresh("named");
    rewrite(&b, MANIFEST, &text_edit("\"file\": \"evo.dat\"", "\"file\": \"evo.bin\""));
    assert!(error(bundle::open(&b)).contains("does not name"));

    let b = fresh("missing");
    std::fs::remove_file(b.join(EVO)).unwrap();
    assert!(error(bundle::open(&b)).contains(EVO));

    // A v1 manifest never has an evo section.
    let b = fresh("v1");
    rewrite(&b, MANIFEST, &text_edit(FORMAT, FORMAT_V1));
    let e = error(bundle::open(&b));
    assert!(e.contains("an evo section in a"), "{e}");
}

/// S2–S5's bundles (manifest v1, no evo section) still open and validate.
#[test]
fn a_v1_bundle_still_opens() {
    let f = fixture();
    let b = make(&f, "v1").unwrap();
    rewrite(&b, MANIFEST, &text_edit(FORMAT, FORMAT_V1));
    assert_eq!(manifest(&b)["format"], FORMAT_V1);
    bundle::validate(&b, &f.golden).unwrap();
    rewrite(&b, MANIFEST, &text_edit(FORMAT_V1, "O9b segment snapshot bundle v3"));
    assert!(error(bundle::open(&b)).contains("not a"));
}
