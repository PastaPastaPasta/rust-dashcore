//! `digest replay` (node program task O9a-2): a recorded node's `blocks.dat` through V, logged by
//! `DigestTap` around the null connector, compared with the node's golden.
//!
//! The main node is `feature_dip3_deterministicmns` node0 of the regtest v24 set: one parameter
//! set (`-dip3params=135:150`, `-budgetparams=10:10:10`), Core's blocks and Core's golden. The
//! replay log has the header keys of spec §5.1 only, so linear mode against the golden is
//! incomplete, with every header key equal in every pair and the other `C.s` keys uncovered;
//! requiring the header keys does not make it clean. A planted change of one golden `time` is
//! reported at its height with the key named.
//!
//! That node ran on wall time, so its chain fits in the future-time window of its genesis
//! attempt. `p2p_invalid_block` node0 ran on mock time: its last block is more than two hours
//! past the earlier attempts, so it replays only with the clock set per block. Every other node
//! of the set replays to its golden too, except the nodes that ran more than one parameter set.
//!
//! What the replay must end on comes from the corpus, not from the golden it is compared with:
//! the tip height and block count of `extract.json`'s summary and the last block of the
//! `blocks.dat` that `extract.json` hashed. A shortened `blocks.dat` and golden pair (Sol r1 F1)
//! fails both the binary and those expectations.
//!
//! The framing, params, corpus and failure cases use edited copies of the node's files; nothing
//! here is synthetic but those edits. A copy that is meant to reach V has its edited files'
//! hashes written into its `extract.json`.

use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use dash_conformance::digest::compare::{Input, compare, project};
use dash_conformance::digest::frames::frames as blocks;
use dash_conformance::digest::replay::{params, replay};
use dash_conformance::digest::store::{Node, Vectors, read, sha256_file};
use dash_statedigest::compare::{FieldId, Mode, Options, Report, Verdict, What};
use dash_statedigest::{Class, FIELDS, Hash256, Kind, Origin, Record, Value, parse_line};
use rayon::prelude::*;

/// The `C.s` keys an emitter computes from the header and the index entry (§5.1).
const HEADER_KEYS: [&str; 8] = ["prev", "ver", "mrkl", "time", "bits", "nonce", "work", "ntx"];

/// The nodes that ran more than one parameter set (`other_params_runs`), which the replay
/// refuses (D17).
const MULTI_PARAMS: [&str; 2] = ["feature_evo_shares_activation", "feature_mnehf"];

fn node_of(test: &str) -> Node {
    let nodes = Vectors::regtest_v24().nodes(test).unwrap();
    nodes.into_iter().find(|n| n.name == "node0").expect("node0")
}

fn node() -> Node {
    node_of("feature_dip3_deterministicmns")
}

/// Runs `dash-conformance digest replay --out <out> <node dir>`: exit code, stdout, stderr.
fn replay_cli(node: &Node, out: &Path) -> (i32, String, String) {
    conformance(&[
        "digest".as_ref(),
        "replay".as_ref(),
        "--out".as_ref(),
        out.as_os_str(),
        node.dir.as_os_str(),
    ])
}

/// Runs `dash-conformance <args>`: exit code, stdout, stderr.
fn conformance(args: &[&OsStr]) -> (i32, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_dash-conformance")).args(args).output().unwrap();
    (
        o.status.code().expect("exited by signal"),
        String::from_utf8(o.stdout).unwrap(),
        String::from_utf8(o.stderr).unwrap(),
    )
}

/// A copy of `node` in `dir/<node.test>/<node.name>` with each `(stem, bytes)` of `edits` stored
/// plain (`blocks.dat`, `golden.jsonl`, ...). With `rehash`, `extract.json` takes the edited
/// files' hashes, so the copy passes `Node::verify`; its summary is never changed.
fn edited(node: &Node, dir: &Path, edits: &[(&str, &[u8])], rehash: bool) -> Node {
    let copy = Node {
        dir: dir.join(&node.test).join(&node.name),
        ..node.clone()
    };
    std::fs::create_dir_all(&copy.dir).unwrap();
    for entry in std::fs::read_dir(&node.dir).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().into_owned();
        if !edits.iter().any(|(stem, _)| name.starts_with(stem)) {
            std::fs::copy(entry.path(), copy.dir.join(&name)).unwrap();
        }
    }
    let mut json = extract_json(node);
    for (stem, bytes) in edits {
        let path = copy.dir.join(stem);
        std::fs::write(&path, bytes).unwrap();
        let sha = serde_json::Value::from(sha256_file(&path).unwrap());
        match *stem {
            "blocks.dat" => json["blocks_dat_sha256"] = sha,
            stem => json[stem.trim_end_matches(".jsonl")]["sha256"] = sha,
        }
    }
    if rehash {
        std::fs::write(copy.dir.join("extract.json"), serde_json::to_vec(&json).unwrap()).unwrap();
    }
    copy
}

fn extract_json(node: &Node) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(node.dir.join("extract.json")).unwrap()).unwrap()
}

/// What a replay of `node` must end on, from its corpus and not from its golden: the summary's
/// tip height and golden block count, and the hash of the last block of `blocks.dat`.
struct Expect {
    tip: i64,
    golden_blocks: usize,
    tip_hash: Hash256,
}

fn expect(node: &Node) -> Expect {
    let summary = &extract_json(node)["summary"];
    let p = params(node).unwrap();
    let blocks = blocks(&read(&node.blocks()).unwrap(), p.message_start).unwrap();
    let last = blocks.last().expect("a block").hash().to_string();
    Expect {
        tip: summary["tip"].as_i64().unwrap(),
        golden_blocks: summary["blocks"].as_u64().unwrap() as usize,
        tip_hash: Hash256::from_hex(last.as_bytes()).unwrap(),
    }
}

/// Whether `node`'s golden ends on `e`: its last height, block and number of heights.
fn golden_agrees(node: &Node, e: &Expect) -> Result<(), String> {
    let chain = project(&node.golden()).map_err(|e| e.to_string())?.chain;
    let last = chain.last().ok_or("an empty golden")?;
    let have = (last.h, &last.b, chain.len());
    let want = (e.tip, &e.tip_hash, e.golden_blocks);
    if have == want {
        Ok(())
    } else {
        Err(format!("golden {have:?}, corpus {want:?}"))
    }
}

/// The frames of a `blocks.dat`, each with its magic and size.
fn frames(bytes: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut rest = bytes;
    while !rest.is_empty() {
        let size = 8 + u32::from_le_bytes(rest[4..8].try_into().unwrap()) as usize;
        let (frame, next) = rest.split_at(size);
        out.push(frame);
        rest = next;
    }
    out
}

fn tempdir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

fn header_fields() -> Vec<FieldId> {
    HEADER_KEYS.iter().map(|k| FieldId::find(Kind::Connect, Class::Semantic, k).unwrap()).collect()
}

/// The `C.s` keys other than the header keys, in spec order.
fn other_fields() -> Vec<FieldId> {
    FIELDS
        .iter()
        .filter(|f| {
            f.kind == Kind::Connect && f.class == Class::Semantic && !HEADER_KEYS.contains(&f.key)
        })
        .map(|f| FieldId::find(f.kind, f.class, f.key).unwrap())
        .collect()
}

fn linear(left: &Path, right: &Path) -> Report {
    linear_from(left, right, None)
}

fn linear_from(left: &Path, right: &Path, from: Option<i64>) -> Report {
    let mut inputs = [Input::open(left).unwrap(), Input::open(right).unwrap()];
    let mut opts = Options::new(Mode::Linear);
    opts.require = header_fields();
    opts.from = from;
    compare(&mut inputs, opts).unwrap()
}

/// Replays `node` into `out` and checks it against the corpus and the golden: V's tip and the
/// golden's last block are the corpus's (`expect`), and every header key is equal at every
/// height the golden has. A node that joined the test late starts recording above genesis (D5),
/// so the comparison starts at the golden's first height.
fn replays_onto_golden(node: &Node, out: &Path) {
    let e = expect(node);
    let replayed = replay(node, out).unwrap_or_else(|e| panic!("{}: {e:#}", node.label()));
    assert_eq!((replayed.tip.0 as i64, &replayed.tip.1), (e.tip, &e.tip_hash), "{}", node.label());
    assert_eq!(replayed.blocks as i64, e.tip, "{}: every block but genesis", node.label());
    golden_agrees(node, &e).unwrap_or_else(|why| panic!("{}: {why}", node.label()));
    let first = e.tip + 1 - e.golden_blocks as i64;
    let report = linear_from(out, &node.golden(), Some(first));
    assert!(report.mismatches.is_empty(), "{}: {:?}", node.label(), report.first());
    assert!(report.skipped.is_empty(), "{}: {:?}", node.label(), report.skipped);
    assert_eq!(report.pairs, e.golden_blocks as u64, "{}", node.label());
    for field in header_fields() {
        let c = &report.coverage[&field];
        assert_eq!(
            (c.equal, c.differ, c.absent()),
            (report.pairs, 0, 0),
            "{}: {field}",
            node.label()
        );
    }
}

#[test]
fn a_recorded_chain_replays_through_v_onto_its_golden() {
    let dir = tempdir();
    let node = node();
    let out = dir.path().join("replay.jsonl");
    let replayed = replay(&node, &out).unwrap();

    // V ends on the node's final chain.
    let golden = project(&node.golden()).unwrap();
    let last = golden.chain.last().unwrap();
    assert_eq!(replayed.tip.0 as i64, last.h);
    assert_eq!(replayed.tip.1, last.b);
    assert_eq!(replayed.blocks, golden.chain.len() - 1, "every block but genesis");

    // The log: one H, then a tip C per block with the header keys and seq only.
    let text = std::fs::read(&out).unwrap();
    let records: Vec<Record> =
        text.split_inclusive(|&b| b == b'\n').map(|l| parse_line(l).unwrap()).collect();
    assert!(
        matches!(&records[0], Record::Header(h) if h.chain == dash_statedigest::Chain::Regtest)
    );
    assert_eq!(records.len(), golden.chain.len() + 1);
    for (i, r) in records[1..].iter().enumerate() {
        let Record::Outcome(o) = r else {
            panic!("record {i} is not an outcome")
        };
        assert_eq!((o.kind(), o.id.org, o.id.h), (Kind::Connect, Origin::Tip, i as i64));
    }

    // Linear mode against the golden: every header key equal in every pair, nothing differing,
    // the other keys uncovered, so incomplete even with the header keys required.
    let report = linear(&out, &node.golden());
    assert!(report.mismatches.is_empty(), "{:?}", report.first());
    assert_eq!(report.pairs, golden.chain.len() as u64);
    assert!(report.skipped.is_empty());
    for field in header_fields() {
        let c = &report.coverage[&field];
        assert_eq!((c.equal, c.differ, c.absent()), (report.pairs, 0, 0), "{field}");
    }
    assert_eq!(report.uncovered, other_fields());
    assert_eq!(report.verdict, Verdict::Incomplete);
}

#[test]
fn a_planted_golden_time_is_reported_against_the_replay() {
    let dir = tempdir();
    let node = node();
    let out = dir.path().join("replay.jsonl");
    replay(&node, &out).unwrap();

    let golden = node.golden();
    let mut lines: Vec<Vec<u8>> =
        read(&golden).unwrap().split_inclusive(|&b| b == b'\n').map(<[u8]>::to_vec).collect();
    let chain = project(&golden).unwrap().chain;
    let slot = &chain[chain.len() / 2];
    let Record::Outcome(mut c) = parse_line(&lines[slot.at - 1]).unwrap() else {
        panic!()
    };
    let Some(Value::Int(time)) = c.s().get("time").cloned() else {
        panic!("no time")
    };
    c.s_mut().set("time", Value::Int(time + 1)).unwrap();
    lines[slot.at - 1] = Record::Outcome(c).to_line();
    let mutated = dir.path().join("mutated.jsonl");
    std::fs::write(&mutated, lines.concat()).unwrap();

    let report = linear(&out, &mutated);
    assert_eq!(report.verdict, Verdict::Diverged);
    let first = report.first().unwrap();
    assert_eq!((first.h, first.b()), (slot.h, &slot.b));
    let What::Keys(diffs) = &first.what else {
        panic!("{:?}", first.what)
    };
    let names: Vec<String> = diffs.iter().map(|d| d.field.to_string()).collect();
    assert_eq!(names, ["C.s.time"]);

    // The binary says the same.
    let require = format!("--require={}", HEADER_KEYS.join(","));
    let run = |right: &Path| {
        let (code, stdout, _) = conformance(&[
            "digest".as_ref(),
            "compare".as_ref(),
            "--no-dump".as_ref(),
            require.as_ref(),
            out.as_os_str(),
            right.as_os_str(),
        ]);
        (code, stdout)
    };
    let (code, stdout) = run(&mutated);
    assert_eq!(code, 1, "{stdout}");
    assert!(
        stdout.contains(&format!(
            "first mismatch at height {} block {}: keys C.s.time\n",
            slot.h, slot.b
        )),
        "{stdout}"
    );
    let (code, stdout) = run(&golden);
    assert_eq!(code, 3, "{stdout}");
    assert!(stdout.contains("uncovered (never passed): "), "{stdout}");
}

#[test]
fn the_binary_replays_the_same_bytes() {
    let dir = tempdir();
    let node = node();
    let lib = dir.path().join("lib.jsonl");
    let replayed = replay(&node, &lib).unwrap();
    let bin = dir.path().join("bin.jsonl");
    let (code, stdout, stderr) = replay_cli(&node, &bin);
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert_eq!(
        stdout,
        format!(
            "replayed {}: {} blocks, tip {} {}\n",
            node.label(),
            replayed.blocks,
            replayed.tip.0,
            replayed.tip.1
        )
    );
    let bytes = std::fs::read(&lib).unwrap();
    assert_eq!(std::fs::read(&bin).unwrap(), bytes);

    // The output is never overwritten.
    let (code, _, stderr) = replay_cli(&node, &bin);
    assert_eq!(code, 2);
    assert!(stderr.contains(&format!("creating {}", bin.display())), "{stderr}");
    assert_eq!(std::fs::read(&bin).unwrap(), bytes);

    // A relative node path names its test: `node0` from the test's directory.
    let rel = dir.path().join("rel.jsonl");
    let o = Command::new(env!("CARGO_BIN_EXE_dash-conformance"))
        .args(["digest", "replay", "--out"])
        .arg(&rel)
        .arg(&node.name)
        .current_dir(node.dir.parent().unwrap())
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(std::fs::read(&rel).unwrap(), bytes);
}

#[test]
fn a_mocktime_chain_replays_only_on_the_recorded_clock() {
    let dir = tempdir();
    replays_onto_golden(&node_of("p2p_invalid_block"), &dir.path().join("replay.jsonl"));
}

/// Every node of the set: the ones that ran one parameter set replay onto their goldens, and the
/// others are refused.
#[test]
fn every_stored_node_replays_onto_its_golden() {
    let dir = tempdir();
    let vectors = Vectors::regtest_v24();
    let nodes: Vec<Node> =
        vectors.tests().unwrap().iter().flat_map(|t| vectors.nodes(t).unwrap()).collect();
    let refused: Vec<String> = nodes
        .par_iter()
        .filter_map(|node| {
            let out = dir.path().join(format!("{}-{}.jsonl", node.test, node.name));
            if MULTI_PARAMS.contains(&node.test.as_str()) {
                let err = replay(node, &out).unwrap_err().to_string();
                assert!(err.contains("more than one parameter set"), "{err}");
                assert!(!out.exists());
                return Some(node.label());
            }
            replays_onto_golden(node, &out);
            None
        })
        .collect();
    assert_eq!(refused.len(), 4, "{refused:?}");
    assert!(nodes.len() > 70, "{} nodes", nodes.len());
}

/// What V refuses ends the replay with V's reason, and the output is removed. A repeated or
/// duplicated genesis never reaches V: `blocks_dat_must_hold_genesis_first_and_only_there`.
#[test]
fn a_block_v_does_not_connect_fails_the_replay() {
    let dir = tempdir();
    let node = node_of("p2p_invalid_block");
    let p = params(&node).unwrap();
    let bytes = read(&node.blocks()).unwrap();
    let frames = frames(&bytes);
    let tail = frames.len() - 1;
    let cases: [(&str, Vec<u8>, &str); 4] = [
        ("dropped", [&frames[..50], &frames[51..]].concat().concat(), "block 50"),
        (
            "mutated",
            {
                // A transaction's last byte (a lock time) changes the merkle root, not the header.
                let mut f = frames[50].to_vec();
                *f.last_mut().unwrap() ^= 1;
                [&frames[..50].concat()[..], &f, &frames[51..].concat()[..]].concat()
            },
            "bad-txnmrklroot",
        ),
        ("repeated", [&bytes[..], frames[tail]].concat(), "not the last block"),
        ("duplicate", [&frames[..=tail].concat()[..], frames[5]].concat(), "not the last block"),
    ];
    for (name, blocks_dat, why) in cases {
        let sub = dir.path().join(name);
        let copy = edited(&node, &sub, &[("blocks.dat", &blocks_dat)], true);
        // Each edit keeps the framing: V, not the reader, refuses it.
        blocks(&blocks_dat, p.message_start).unwrap();
        let out = sub.join("replay.jsonl");
        let err = format!("{:#}", replay(&copy, &out).unwrap_err());
        assert!(err.contains(why), "{name}: {err}");
        assert!(!out.exists(), "{name}: the output is left");
    }
}

/// The stored files must be the ones `extract.json` hashed: an edit of any replay input is
/// refused before V starts, with exit 2 and no output.
#[test]
fn a_file_extract_json_did_not_hash_is_refused() {
    let dir = tempdir();
    let node = node_of("p2p_invalid_block");
    for stem in ["blocks.dat", "digest.jsonl", "decisions.jsonl"] {
        let mut bytes = read(&node.dir.join(format!("{stem}.zst"))).unwrap();
        bytes.push(b'\n');
        let copy = edited(&node, &dir.path().join(stem), &[(stem, &bytes)], false);
        let out = dir.path().join(format!("{stem}.out"));
        let (code, _, stderr) = replay_cli(&copy, &out);
        assert_eq!(code, 2, "{stem}: {stderr}");
        let want = format!("{}: sha256 ", copy.dir.join(stem).display());
        assert!(stderr.contains(&want) && stderr.contains("extract.json says"), "{stem}: {stderr}");
        assert!(!out.exists(), "{stem}");
    }
}

/// Sol r1's probe: `blocks.dat` cut to genesis and the golden cut to its `H` and genesis `C`.
/// The binary refuses the pair (the hashes), and with the hashes rewritten it still fails (the
/// summary tip). The expectations the all-node test uses reject the cut golden too.
#[test]
fn a_shortened_blocks_dat_and_golden_pair_fails() {
    let dir = tempdir();
    let node = node();
    let bytes = read(&node.blocks()).unwrap();
    let genesis_only = frames(&bytes)[0].to_vec();
    let golden = read(&node.golden()).unwrap();
    let cut: Vec<u8> = golden.split_inclusive(|&b| b == b'\n').take(2).collect::<Vec<_>>().concat();
    let edits: [(&str, &[u8]); 2] = [("blocks.dat", &genesis_only), ("golden.jsonl", &cut)];

    for (label, rehash, want) in [
        ("plain", false, "sha256"),
        ("rehashed", true, "V's tip height 0 is not extract.json's summary tip 245"),
    ] {
        let copy = edited(&node, &dir.path().join(label), &edits, rehash);
        let out = dir.path().join(format!("{label}.jsonl"));
        let (code, _, stderr) = replay_cli(&copy, &out);
        assert_eq!(code, 2, "{label}: {stderr}");
        assert!(stderr.contains(want), "{label}: {stderr}");
        assert!(!out.exists(), "{label}");
        assert!(golden_agrees(&copy, &expect(&copy)).is_err(), "{label}");
    }
}

/// The replay must end on the golden's last block, which `Node::verify` binds to the corpus: a
/// golden whose last block is another (hashes rewritten) is refused after V, with no output.
#[test]
fn a_replay_that_does_not_end_on_the_golden_tip_fails() {
    let dir = tempdir();
    let node = node_of("p2p_invalid_block");
    // The golden's last block, renamed in its tip `C` (the slot the projection keeps).
    let chain = project(&node.golden()).unwrap().chain;
    let (slot, genesis) = (chain.last().unwrap(), chain[0].b);
    let mut lines: Vec<Vec<u8>> = read(&node.golden())
        .unwrap()
        .split_inclusive(|&b| b == b'\n')
        .map(<[u8]>::to_vec)
        .collect();
    let Record::Outcome(mut c) = parse_line(&lines[slot.at - 1]).unwrap() else {
        panic!("the slot is not an outcome")
    };
    c.id.b = genesis;
    lines[slot.at - 1] = Record::Outcome(c).to_line();
    let edited_golden = lines.concat();
    let last = &slot.b;
    let copy = edited(&node, dir.path(), &[("golden.jsonl", &edited_golden)], true);
    let out = dir.path().join("replay.jsonl");
    let (code, _, stderr) = replay_cli(&copy, &out);
    assert_eq!(code, 2, "{stderr}");
    assert_eq!(stderr, format!("error: V's tip {last} is not the golden's last block {genesis}\n"));
    assert!(!out.exists());
}

/// `blocks.dat` holds genesis first and nowhere else (Sol r1 F2): an empty file, a file without
/// genesis first and a second genesis are refused before V starts, with exit 2 and no output.
#[test]
fn blocks_dat_must_hold_genesis_first_and_only_there() {
    let dir = tempdir();
    let node = node();
    let bytes = read(&node.blocks()).unwrap();
    let f = frames(&bytes);
    let cases: [(&str, Vec<u8>, &str); 5] = [
        ("empty", Vec::new(), "blocks.dat holds no block"),
        ("no-genesis", f[1..].concat(), "blocks.dat block 0 is not genesis"),
        ("genesis-last", [&f[1..], &f[..1]].concat().concat(), "blocks.dat block 0 is not genesis"),
        (
            "genesis-again",
            [&f[..5], &f[..1], &f[5..]].concat().concat(),
            "blocks.dat block 5 is genesis again",
        ),
        (
            "genesis-appended",
            [&bytes[..], f[0]].concat(),
            &format!("blocks.dat block {} is genesis again", f.len()),
        ),
    ];
    for (name, blocks_dat, why) in &cases {
        let copy = edited(&node, &dir.path().join(name), &[("blocks.dat", blocks_dat)], true);
        let out = dir.path().join(format!("{name}.jsonl"));
        let (code, _, stderr) = replay_cli(&copy, &out);
        assert_eq!(code, 2, "{name}: {stderr}");
        assert_eq!(stderr, format!("error: {why}\n"), "{name}");
        assert!(!out.exists(), "{name}");
    }
}

/// A failed log write is an AbortNode (§8.3): the replay reports it, not V's lost reply, and the
/// output is removed. The file size limit makes the write fail; SIGXFSZ is ignored so that the
/// write returns the error.
#[test]
fn a_failed_log_write_is_reported_as_the_abort() {
    let dir = tempdir();
    let node = node_of("p2p_invalid_block");
    let out = dir.path().join("replay.jsonl");
    let o = Command::new("bash")
        .args(["-c", "trap '' XFSZ; ulimit -f 2; exec \"$0\" \"$@\""])
        .arg(env!("CARGO_BIN_EXE_dash-conformance"))
        .args(["digest", "replay", "--out"])
        .arg(&out)
        .arg(&node.dir)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("V stopped on an AbortNode: statedigest: writing the digest log"),
        "{stderr}"
    );
    assert!(!out.exists());
}

#[test]
fn blocks_dat_framing_errors_are_refused() {
    let node = node();
    let p = params(&node).unwrap();
    let bytes = read(&node.blocks()).unwrap();
    let all = blocks(&bytes, p.message_start).unwrap();
    assert_eq!(all[0].hash(), dashd_chainstate::chain::genesis_block(&p).hash());
    let first = 8 + u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;

    let err = |bytes: &[u8]| match blocks(bytes, p.message_start) {
        Ok(_) => panic!("accepted"),
        Err(e) => e.to_string(),
    };
    let mut magic = bytes.clone();
    magic[0] ^= 1;
    assert_eq!(err(&magic), "blocks.dat offset 0: not the network magic");
    // The offset is the frame's: the second frame starts where the first ends.
    let mut second = bytes.clone();
    second[first] ^= 1;
    assert_eq!(err(&second), format!("blocks.dat offset {first}: not the network magic"));
    assert_eq!(err(&bytes[..5]), "blocks.dat offset 0: a cut frame header");
    assert_eq!(err(&bytes[..first + 5]), format!("blocks.dat offset {first}: a cut frame header"));
    assert_eq!(err(&bytes[..first - 1]), "blocks.dat offset 0: a cut block");
    let mut longer = bytes[..first].to_vec();
    longer[4..8].copy_from_slice(&(first as u32 - 8 + 1).to_le_bytes());
    longer.push(0);
    assert_eq!(err(&longer), "blocks.dat offset 0: bytes after the block");
    assert_eq!(blocks(&bytes[..first], p.message_start).unwrap().len(), 1);
}

#[test]
fn params_come_from_extract_json() {
    let node = node();
    let p = params(&node).unwrap();
    // `-dip3params=135:150` (Core's default regtest activation is far lower).
    assert_eq!(p.consensus.dip0003_height.to_string(), "135");

    let dir = tempdir();
    let base = extract_json(&node);
    for (key, value, why) in [
        ("other_params_runs", serde_json::json!(["x"]), "more than one parameter set"),
        ("chain_args", serde_json::json!(["-nosuchoption=1"]), "unknown chain option"),
        ("chain_args", serde_json::Value::Null, "no chain_args"),
    ] {
        let copy = Node {
            dir: dir.path().join(why.replace(' ', "-")),
            ..node.clone()
        };
        std::fs::create_dir(&copy.dir).unwrap();
        let mut json = base.clone();
        json[key] = value;
        std::fs::write(copy.dir.join("extract.json"), serde_json::to_vec(&json).unwrap()).unwrap();
        let err = params(&copy).unwrap_err().to_string();
        assert!(err.contains(why), "{why}: {err}");
    }
}
