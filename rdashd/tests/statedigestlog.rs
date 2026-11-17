//! Production digest logging against an O8-patched v24 dashd (`DASHD_V24_PATH`).
//! `STATEDIGEST_CONFORMANCE` selects the companion conformance binary when it is not beside rdashd.
#![cfg(unix)]

mod common;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::copier::copy_stopped;
use common::mine::{Mined, coinbase_paying, next_with, parse_hex};
use common::node::{Node, Storage, genesis};
use common::*;
use dash_consensus::primitives::decode_header;
use serde_json::Value;

const SHARED: [&str; 16] = [
    "prev", "ver", "mrkl", "time", "bits", "nonce", "work", "ntx", "undo", "mnl", "qc", "cp",
    "ehf", "bls", "bur", "dep",
];
const GOLDEN: [&str; 6] = ["muhash", "txouts", "amount", "unspendable", "bi", "stats"];
const OFFLINE: [&str; 5] =
    ["-connect=0", "-dnsseed=0", "-fixedseeds=0", "-checklevel=2", "-rpcdoccheck=0"];
const DATA: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/../dashd-chainstate/tests/data/v19-window/r2");

fn conformance() -> PathBuf {
    env::var_os("STATEDIGEST_CONFORMANCE")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(RDASHD).parent().unwrap().join("dash-conformance"))
}

fn digest(node: &Node) -> PathBuf {
    node.datadir.join("regtest/digest.jsonl")
}

fn records(path: &Path) -> Vec<Value> {
    read(path).lines().map(|line| serde_json::from_str(line).unwrap()).collect()
}

fn compare(left: &Path, right: &Path, tip: usize, golden: bool, report: &Path) {
    let fields: Vec<_> = SHARED
        .iter()
        .chain(if golden {
            &GOLDEN[..]
        } else {
            &[]
        })
        .copied()
        .collect();
    let out = output({
        let mut cmd = Command::new(conformance());
        cmd.args(["digest", "compare"]).arg(left).arg(right).args([
            "--mode",
            "linear",
            "--from",
            "0",
            "--to",
            &tip.to_string(),
            "--require",
            &fields.join(","),
            "--no-dump",
        ]);
        cmd
    });
    let text = String::from_utf8(out.stdout).unwrap();
    fs::write(report, &text).unwrap();
    assert_eq!(
        out.status.code(),
        Some(if golden {
            0
        } else {
            3
        }),
        "{text}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.contains(&format!("{} pairs, heights 0..={tip}:", tip + 1)), "{text}");
    for key in &fields {
        let name = format!("C.s.{key}");
        let row = text.lines().find(|line| line.split_whitespace().next() == Some(&name)).unwrap();
        assert_eq!(
            row.split_whitespace().skip(1).collect::<Vec<_>>().join(" "),
            format!("{} / 0 / 0, 0, 0", tip + 1),
            "{row}"
        );
    }
    eprintln!(
        "{}: {} pairs, {} semantic keys equal at every height",
        report.display(),
        tip + 1,
        fields.len()
    );
}

fn extract(core: &Node, home: &Path, flags: &[&str], out: &Path) {
    let mut copy = Node::new("frozen-core", &core.binary, home, flags);
    copy_stopped(&core.datadir, &copy.datadir);
    copy.flags.extend(OFFLINE.map(str::to_owned));
    copy.flags.push("-coinstatsindex=1".to_owned());
    copy.start(0);
    let port = copy.run.as_ref().unwrap().1;
    let result = output({
        let mut cmd = Command::new(conformance());
        cmd.args([
            "digest",
            "extract",
            "--rpc-url",
            &format!("http://127.0.0.1:{port}"),
            "--rpc-user",
            "u",
            "--rpc-password",
            "p",
            "--digest",
        ])
        .arg(digest(core))
        .arg("--out")
        .arg(out)
        .arg("--blocks-dir")
        .arg(copy.datadir.join("regtest/blocks"))
        .arg("--summary")
        .arg(out.with_extension("summary.json"));
        cmd
    });
    copy.stop();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        stderr(&result)
    );
}

fn differential(
    label: &str,
    flags: &[&str],
    blocks: impl FnOnce(&Node) -> Vec<Mined>,
    split: Option<usize>,
    witness: bool,
) {
    let home = RetainedHome::new(label);
    let mut core = Node::new("core", &require_dashd(), home.path(), flags);
    let mut ours = Node::rdashd(home.path(), flags, Storage::Live);
    for node in [&mut core, &mut ours] {
        node.flags.extend(OFFLINE.map(str::to_owned));
        node.flags.push("-statedigestlog=digest.jsonl".to_owned());
    }
    core.flags.push("-coinstatsindex=1".to_owned());
    core.start(0);
    ours.start(0);
    let blocks = blocks(&core);
    for (index, block) in blocks.iter().enumerate() {
        for node in [&mut core, &mut ours] {
            assert_eq!(node.submit(block), "null", "height {}", index + 1);
            if split == Some(index + 1) {
                node.stop();
                let before = fs::read(digest(node)).unwrap();
                node.start(0);
                assert!(fs::read(digest(node)).unwrap().starts_with(&before));
            }
        }
        assert_eq!(ours.best(), core.best());
    }
    core.stop();
    ours.stop();
    let golden = home.path().join("golden.jsonl");
    extract(&core, home.path(), flags, &golden);
    compare(&digest(&core), &digest(&ours), blocks.len(), false, &home.path().join("raw.txt"));
    compare(&golden, &digest(&ours), blocks.len(), true, &home.path().join("golden.txt"));
    let ours_records = records(&digest(&ours));
    assert_eq!(
        ours_records.iter().filter(|r| r["k"] == "H").count(),
        if split.is_some() {
            2
        } else {
            1
        }
    );
    let headers: Vec<_> = ours_records
        .iter()
        .enumerate()
        .filter_map(|(index, record)| (record["k"] == "H").then_some(index))
        .chain(std::iter::once(ours_records.len()))
        .collect();
    let mut runs = std::collections::BTreeSet::new();
    for bounds in headers.windows(2) {
        let segment = &ours_records[bounds[0]..bounds[1]];
        assert!(runs.insert(segment[0]["run"].as_str().unwrap()), "duplicate H.run");
        let attempts: Vec<_> =
            segment[1..].iter().map(|record| record["x"]["seq"].as_u64().unwrap()).collect();
        assert!(!attempts.is_empty());
        assert_eq!(attempts, (1..=attempts.len() as u64).collect::<Vec<_>>());
    }
    assert!(ours_records.iter().filter(|r| r["k"] == "C").all(|r| r["o"] == serde_json::json!({})));
    assert!(!ours_records.iter().any(|r| r["k"] == "A"));
    if witness {
        let records = records(&golden);
        let first = records.iter().find(|r| r["k"] == "C" && r["h"] == 1).unwrap();
        assert_eq!(first["s"]["bi"][5].as_i64(), Some(10_000_000_000));
        assert_eq!(first["s"]["bi"][6].as_i64(), Some(100_000_000));
    }
    if let Some(root) = env::var_os("STATEDIGEST_REPORT_DIR") {
        let root = PathBuf::from(root).join(label);
        fs::create_dir_all(&root).unwrap();
        for (from, name) in [
            (digest(&core), "core.jsonl"),
            (digest(&ours), "rdashd.jsonl"),
            (golden.clone(), "golden.jsonl"),
            (home.path().join("raw.txt"), "raw.txt"),
            (home.path().join("golden.txt"), "golden.txt"),
            (golden.with_extension("summary.json"), "summary.json"),
        ] {
            fs::copy(from, root.join(name)).unwrap();
        }
    }
}

fn r2() -> (Vec<String>, Vec<Mined>) {
    let data = Path::new(DATA);
    let summary: Value = serde_json::from_str(&read(&data.join("summary.json"))).unwrap();
    let mut flags: Vec<_> = summary["chain_args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap().to_owned())
        .collect();
    let blocks: Vec<_> = read(&data.join("chain.txt"))
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .map(|hex| {
            let raw = parse_hex(&hex[..160]);
            let header = decode_header(&mut raw.as_slice()).unwrap().into_value();
            Mined {
                hex: hex.to_owned(),
                hash: *header.hash().as_bytes(),
            }
        })
        .collect();
    assert_eq!(blocks.len(), 290);
    let header = parse_hex(&blocks.last().unwrap().hex[..160]);
    let time = u32::from_le_bytes(header[68..72].try_into().unwrap());
    flags.push(format!("-mocktime={}", time + 60));
    (flags, blocks)
}

#[test]
fn activation_chain_digest_matches_core() {
    let (flags, blocks) = r2();
    let flags: Vec<_> = flags.iter().map(String::as_str).collect();
    differential("statedigest-r2", &flags, |_| blocks, None, false);
}

#[test]
fn activation_chain_digest_matches_core_across_restart() {
    let (flags, blocks) = r2();
    let flags: Vec<_> = flags.iter().map(String::as_str).collect();
    differential("statedigest-r2-restart", &flags, |_| blocks, Some(235), false);
}

#[test]
fn burn_and_unclaimed_rewards_match_core_across_restart() {
    differential(
        "statedigest-witness",
        &[
            "-dip3params=1000:1000",
            "-testactivationheight=v20@1000",
            "-testactivationheight=mn_rr@1000",
        ],
        |core| {
            let mut prev = genesis(core);
            (1..=5)
                .map(|height| {
                    let mut cb =
                        coinbase_paying(height.into(), b"seed", &[39_900_000_000, 10_000_000_000]);
                    let last_script = cb.len() - 5;
                    assert_eq!(cb[last_script], 0x51);
                    cb[last_script] = 0x6a;
                    let block = next_with(&prev, height, cb);
                    prev = block.hash;
                    block
                })
                .collect()
        },
        Some(2),
        true,
    );
}
