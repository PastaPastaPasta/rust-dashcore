//! Segment snapshot bundles (O9b S2): `segment bundle` and `segment validate` over a synthetic
//! regtest chain of 110 blocks (block 102 spends block 1's coinbase), its log from genesis as
//! the golden, and the snapshot a run to 100 saves (CH3a D165).
//!
//! - A bundle validates, and `digest l2 --start-utxo` runs its window 101..=110 from it.
//! - Each check refuses on its own: a snapshot whose sidecar records another scan, a golden whose
//!   MuHash at the base differs, a changed byte in the bundle, a file the checksums do not list,
//!   checksums listing a file twice or not every file, a snapshot or manifest that is not the
//!   sidecar's (even with its checksum rewritten), an evo section, and an output directory that
//!   exists. A refused `bundle` leaves no directory.

// Shared with the digest tests; this file uses the chain helpers only.
#[allow(dead_code)]
mod digest_common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use dash_conformance::digest::blkdir::BlockDir;
use dash_conformance::digest::l2::{self, Options, Source};
use dash_conformance::digest::utxo::sidecar_path;
use dash_conformance::segment::bundle::{self, MANIFEST, SIDECAR, SUMS, UTXO};
use dash_consensus::primitives::{Tx, core_serialize};
use dash_params::{ChainParams, ChainParamsBuilder, NetworkKind, Release};
use dash_statedigest::Chain;
use dashd_chainstate::chain::genesis_block;
use digest_common::{coinbase_sig, decode, mine, tempdir};

const WINDOW: u32 = 110;
const BASE: u32 = 100;
const SPEND_AT: u32 = 102;
const COIN: i64 = 100_000_000;

fn regtest() -> ChainParams {
    ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0).build().unwrap()
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
fn chain(p: &ChainParams) -> tempfile::TempDir {
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

fn run(dir: &BlockDir, p: &ChainParams, opts: &Options, out: &Path, name: &str, to: u32) {
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
    p: ChainParams,
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
    bundle::make(&f.snapshot, &f.golden, "test transition", &at).map(|m| m.dir)
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
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(b.join(MANIFEST)).unwrap()).unwrap();
    assert_eq!(manifest["evo"], serde_json::Value::Null);
    assert_eq!(manifest["base"]["height"], BASE);

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
        error(bundle::make(&f.snapshot, &bad, "t", &f.out.path().join("c"))).contains("muhash")
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
    let rewrite = |b: &Path, file: &str, edit: &dyn Fn(Vec<u8>) -> Vec<u8>| {
        use dash_conformance::digest::store::sha256_hex as digest;
        let old = std::fs::read(b.join(file)).unwrap();
        let new = edit(old.clone());
        assert_ne!(new, old);
        std::fs::write(b.join(file), &new).unwrap();
        let sums = std::fs::read_to_string(b.join(SUMS)).unwrap();
        std::fs::write(b.join(SUMS), sums.replace(&digest(&old), &digest(&new))).unwrap();
    };
    let text_edit = |from: &'static str, to: &'static str| {
        move |bytes: Vec<u8>| String::from_utf8(bytes).unwrap().replacen(from, to, 1).into_bytes()
    };
    for (name, file, edit, want) in [
        ("chain", MANIFEST, text_edit("\"regtest\"", "\"test\""), "the manifest's chain"),
        ("coins", MANIFEST, text_edit("\"coins\": 100", "\"coins\": 99"), "utxo section"),
        ("evo", MANIFEST, text_edit("\"evo\": null", "\"evo\": {}"), "an evo section"),
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
