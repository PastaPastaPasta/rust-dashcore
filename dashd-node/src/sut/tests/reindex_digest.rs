//! Compares every stored-chain replay digest with Core's standard golden.

use std::env;
use std::fs::{self, DirBuilder, File};
use std::io::Write;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use dash_consensus::Block;
use dash_consensus::coins::stats::CoinStatsHashType;
use dash_consensus::primitives::{core_serialize, decode_block};
use dash_statedigest::{Hash256, Kind, Origin, Record};
use dashd_chainstate::connect::{DigestSink, MuHashAt};
use serde_json::{Value, json};

use super::state_hash::{ALL_C_S, DigestRun, linear_clean, read_records};
use super::{NodeSut, submit, ui, view_tip};

#[path = "../../../tests/v24/mod.rs"]
mod v24;

fn input(name: &str) -> PathBuf {
    let path = PathBuf::from(env::var_os(name).unwrap_or_else(|| panic!("{name} is required")));
    assert!(path.is_absolute(), "{name} must be absolute");
    path
}

fn private_dir(path: &Path) {
    DirBuilder::new().mode(0o700).create(path).unwrap();
}

fn sha256(bytes: &[u8]) -> String {
    dash_sha256::sha256(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn wait(what: &str, done: impl Fn() -> bool) {
    let started = Instant::now();
    while !done() {
        assert!(started.elapsed() < Duration::from_secs(600), "timed out: {what}");
        thread::sleep(Duration::from_millis(20));
    }
}

fn chain_records(path: &Path, blocks: &[Arc<Block>]) {
    let mut next = 0;
    for record in read_records(path) {
        match record {
            Record::Header(_) | Record::Sparse(_) => {}
            Record::Outcome(o) => {
                assert_eq!(o.kind(), Kind::Connect, "only successful replay connects");
                assert_eq!(o.id.org, Origin::Tip, "startup ABC's origin");
                assert_eq!(o.id.h, next as i64, "no repeated or skipped heights");
                assert_eq!(o.id.b.to_hex(), blocks[next].hash().to_string());
                next += 1;
            }
            other => panic!("unexpected digest record {other:?}"),
        }
    }
    assert_eq!(next, blocks.len(), "complete replay stream");
}

fn reopen_with_digest(
    mut sut: NodeSut,
    dir: &Path,
    args: &[&str],
    mocktime: i64,
    sink: Box<dyn DigestSink>,
) -> NodeSut {
    sut.steps.shutdown();
    let coins = Arc::clone(&sut.coins);
    drop(sut);
    coins.restart();
    let mut sut = NodeSut::unopened(dir, args, mocktime, ui(), coins).expect("reopen configured");
    sut.pending_digest = Some(sink);
    sut.open();
    assert!(!sut.digest_pending(), "startup must consume the fresh digest sink");
    assert_eq!(sut.down(), None, "replay must load");
    sut
}

#[test]
#[ignore = "CH5-2 G11 (OD-CH5-54): external Core binary and st3b fixture measurement"]
fn stored_chain_replay_state_digest_matches_core() {
    let started = Instant::now();
    let binary = input("G11_DASHD");
    let extractor = input("G11_CONFORMANCE");
    let fixture = input("G11_FIXTURE").canonicalize().unwrap();
    let output = input("G11_OUT");
    let height: usize = env::var("G11_HEIGHT").expect("G11_HEIGHT is required").parse().unwrap();
    let parent = output.parent().unwrap().canonicalize().unwrap();
    assert!(!parent.starts_with(&fixture), "output must be outside the fixture");
    assert!(!output.exists(), "output must be fresh");
    private_dir(&output);
    let provenance_bytes = fs::read(fixture.join("provenance.json")).unwrap();
    let provenance: Value = serde_json::from_slice(&provenance_bytes).unwrap();
    assert!(height > 0 && height <= provenance["blocks"].as_u64().unwrap() as usize);
    for (name, pin) in provenance["files"].as_object().unwrap() {
        assert_eq!(sha256(&fs::read(fixture.join(name)).unwrap()), pin.as_str().unwrap(), "{name}");
    }
    let raw = fs::read(fixture.join("blocks.bin")).unwrap();
    let mut rest = raw.as_slice();
    let mut blocks = Vec::new();
    for _ in 0..=height {
        let (size, tail) = rest.split_at(4);
        let size = u32::from_le_bytes(size.try_into().unwrap()) as usize;
        let (bytes, tail) = tail.split_at(size);
        let mut input = bytes;
        blocks.push(Arc::new(decode_block(&mut input).unwrap().into_value()));
        assert!(input.is_empty());
        rest = tail;
    }
    let tip = blocks.last().unwrap().hash();
    if height == provenance["blocks"].as_u64().unwrap() as usize {
        assert_eq!(tip.to_string(), provenance["final"].as_str().unwrap());
    }
    let mocktime = i64::from(blocks.last().unwrap().header().time()) + 1;
    let mut args: Vec<String> = provenance["chain_args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    args.extend(
        ["-txindex=0", "-disablewallet", "-connect=0", "-par=1", "-assumevalid=0"]
            .map(str::to_owned),
    );
    let flags: Vec<&str> = args.iter().map(String::as_str).collect();
    let core_dir = output.join("core");
    let rust_dir = output.join("rdashd");
    private_dir(&core_dir);
    private_dir(&rust_dir);
    fs::write(
        output.join("inputs.json"),
        serde_json::to_vec_pretty(&json!({
            "binary": binary, "binary_sha256": sha256(&fs::read(&binary).unwrap()),
            "extractor": extractor, "extractor_sha256": sha256(&fs::read(&extractor).unwrap()),
            "fixture": fixture, "provenance_sha256": sha256(&provenance_bytes),
            "provenance": provenance, "height": height, "tip": tip.to_string(), "args": args,
        }))
        .unwrap(),
    )
    .unwrap();

    let core = v24::Dashd::start_at_with_timeout(
        &binary,
        &core_dir,
        mocktime,
        &[flags.as_slice(), &["-coinstatsindex=0"]].concat(),
        Duration::from_secs(600),
    );
    for block in &blocks[1..] {
        assert_eq!(
            core.call("submitblock", json!([v24::hex(&core_serialize(block.as_ref()))])),
            Value::Null
        );
    }
    assert_eq!(core.call("getbestblockhash", json!([])), tip.to_string());
    drop(core);
    let digest = output.join("core-replay.jsonl");
    let log_flag = format!("-statedigestlog={}", digest.display());
    let core = v24::Dashd::start_at_with_timeout(
        &binary,
        &core_dir,
        mocktime,
        &[flags.as_slice(), &["-coinstatsindex=1", "-reindex-chainstate", &log_flag]].concat(),
        Duration::from_secs(600),
    );
    wait("Core replay tip", || core.call("getbestblockhash", json!([])) == tip.to_string());
    drop(core);
    chain_records(&digest, &blocks);
    let core = v24::Dashd::start_at_with_timeout(
        &binary,
        &core_dir,
        mocktime,
        &[flags.as_slice(), &["-coinstatsindex=1"]].concat(),
        Duration::from_secs(600),
    );
    let golden = output.join("core-golden.jsonl");
    let extract = Command::new(&extractor)
        .args([
            "digest",
            "extract",
            "--rpc-url",
            &core.rpc_url(),
            "--rpc-user",
            "mn0",
            "--rpc-password",
            "mn0",
            "--dense",
            "--digest",
        ])
        .arg(&digest)
        .arg("--out")
        .arg(&golden)
        .arg("--blocks-dir")
        .arg(core_dir.join("regtest/blocks"))
        .arg("--summary")
        .arg(output.join("extract-summary.json"))
        .stdout(Stdio::from(File::create(output.join("extract.stdout")).unwrap()))
        .stderr(Stdio::from(File::create(output.join("extract.stderr")).unwrap()))
        .status()
        .unwrap();
    assert!(
        extract.success(),
        "standard digest extract refused the replay stream: see extract.stderr"
    );
    chain_records(&golden, &blocks);

    let rust_flags = [&["-regtest", "-printtoconsole=0"][..], flags.as_slice()].concat();
    let sut = NodeSut::new(&rust_dir, &rust_flags, mocktime, ui()).expect("population starts");
    for block in &blocks[1..] {
        submit(&sut, block);
    }
    assert_eq!(view_tip(&sut), tip);
    let (run, sink) = DigestRun::new(&rust_dir, MuHashAt::Every);
    let replay_flags = [rust_flags.as_slice(), &["-reindex-chainstate"]].concat();
    let sut = reopen_with_digest(sut, &rust_dir, &replay_flags, mocktime, sink);
    wait("rdashd replay tip", || view_tip(&sut) == tip);
    chain_records(run.path(), &blocks);
    let compared = linear_clean(&run, &golden, 0, height as i64);
    assert_eq!(compared.pairs, (height + 1) as u64);
    for (kind, key, params) in [
        (CoinStatsHashType::HashSerialized, "hash_serialized_2", json!(["hash_serialized_2"])),
        (CoinStatsHashType::MuHash, "muhash", json!(["muhash", null, false])),
    ] {
        let scan = sut
            .chain()
            .unwrap()
            .coins_stats(kind, Arc::new(|| false), None)
            .recv()
            .unwrap()
            .unwrap()
            .wait()
            .unwrap();
        assert_eq!(scan.best_block, tip);
        let core_scan = core.call("gettxoutsetinfo", params);
        let hash = Hash256::from_wire_bytes(scan.stats.hash.unwrap()).to_hex();
        assert_eq!(core_scan["bestblock"], tip.to_string());
        assert_eq!(core_scan[key], hash, "Core chainstate scan {key}");
        assert_eq!(core_scan["txouts"].as_u64(), Some(scan.stats.txouts));
        assert_eq!(core_scan["height"].as_u64(), Some(height as u64));
        assert_eq!(core_scan["transactions"].as_u64(), Some(scan.stats.transactions));
        assert_eq!(core_scan["bogosize"].as_u64(), Some(scan.stats.bogosize));
        let amount = scan.stats.total_amount.unwrap();
        assert!(amount >= 0);
        let amount: Value =
            serde_json::from_str(&format!("{}.{:08}", amount / 100_000_000, amount % 100_000_000))
                .unwrap();
        assert_eq!(core_scan["total_amount"], amount);
        fs::write(
            output.join(format!("tip-{key}.json")),
            serde_json::to_vec_pretty(&core_scan).unwrap(),
        )
        .unwrap();
        if key == "muhash" {
            let last = read_records(&golden)
                .into_iter()
                .filter_map(|r| match r {
                    Record::Outcome(o) => Some(o),
                    _ => None,
                })
                .next_back()
                .unwrap();
            assert_eq!(last.s().get("muhash").unwrap().as_hash().unwrap().to_hex(), hash);
        }
    }
    drop(sut);
    drop(core);
    let line = format!(
        "G11 PASS heights={} fields={} mismatches=0; 16 keys replay vs replay; 6 golden keys: rdashd replay vs Core coinstatsindex; Core tip chainstate scan equal; wall_s={:.3}",
        height + 1,
        ALL_C_S.len(),
        started.elapsed().as_secs_f64()
    );
    writeln!(File::create(output.join("result.txt")).unwrap(), "{line}").unwrap();
    println!("{line}");
}
