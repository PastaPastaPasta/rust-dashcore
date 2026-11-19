//! LQ3c's `qrinfo` fixture against Dash Core v24.0.0: every reply decodes as base-sdk's `QrInfo` at
//! the protocol version it was served at and re-encodes to Core's bytes (70227's through its 70228
//! sibling, see [`replies_decode_at_their_band_and_reencode`]), the evodb rows decode, and the
//! counts are the ones `PROVENANCE.md` states.
//!
//! Fixtures: `tests/data/qrinfo/`, built by `contrib/node/lq3c-qrinfo/capture.py` from Core's
//! `feature_llmq_rotation` regtest chain (provenance: `tests/data/qrinfo/PROVENANCE.md`).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;

use dash_consensus::hash::sha256d;
use dash_coreimport::Hash256;
use dash_coreimport::evodb::{Ancestry, EvoDbImage};
use dash_p2p_core::{CycleInfo, MnListDiff, MnListFormat, ProtocolVersion, QrInfo, QuorumSnapshot};
use dash_primitives::Commitment;
use dash_types::codec::BaseCodec;
use serde_json::Value;

/// `-dip3params=2:2` (the test's dash.conf).
const DIP3_HEIGHT: i32 = 2;
/// `llmq_test_dip0024`, the regtest DIP0024 InstantSend type.
const LLMQ_TYPE: u8 = 103;

fn dir() -> String {
    format!("{}/tests/data/qrinfo", env!("CARGO_MANIFEST_DIR"))
}

fn unhex(hex: &str) -> Vec<u8> {
    (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect()
}

/// A hash as RPC displays it, in internal byte order.
fn display_hash(hex: &str) -> Hash256 {
    let mut hash: Hash256 = unhex(hex).try_into().unwrap();
    hash.reverse();
    hash
}

struct Block {
    height: i32,
    prev: Option<Hash256>,
    have_data: bool,
    status: String,
}

struct Fixture {
    json: Value,
    blocks: HashMap<Hash256, Block>,
    /// The active chain by height.
    active: Vec<Hash256>,
}

impl Ancestry for Fixture {
    fn header(&self, block: &Hash256) -> Option<(i32, Option<Hash256>)> {
        self.blocks.get(block).map(|b| (b.height, b.prev))
    }
}

fn load() -> Fixture {
    let mut text = String::new();
    flate2::read::GzDecoder::new(
        std::fs::File::open(format!("{}/feature_llmq_rotation.json.gz", dir())).unwrap(),
    )
    .read_to_string(&mut text)
    .unwrap();
    let json: Value = serde_json::from_str(&text).unwrap();
    let mut blocks = HashMap::new();
    let mut active = Vec::new();
    for row in json["blocks"].as_array().unwrap() {
        let hash = display_hash(row[0].as_str().unwrap());
        let height = row[1].as_i64().unwrap() as i32;
        let status = row[4].as_str().unwrap().to_owned();
        if status == "active" {
            assert_eq!(active.len() as i32, height, "the active chain is listed by height");
            active.push(hash);
        }
        let block = Block {
            height,
            prev: row[2].as_str().map(display_hash),
            have_data: row[3].as_bool().unwrap(),
            status,
        };
        assert!(blocks.insert(hash, block).is_none(), "each block once");
    }
    Fixture {
        json,
        blocks,
        active,
    }
}

/// `PROVENANCE.md`'s counts table: `| `name` | count |` rows.
fn provenance_counts() -> BTreeMap<String, usize> {
    let text = std::fs::read_to_string(format!("{}/PROVENANCE.md", dir())).unwrap();
    text.lines()
        .filter_map(|line| {
            let cells: Vec<&str> = line.split('|').map(str::trim).collect();
            let name = cells.get(1)?.strip_prefix('`')?.strip_suffix('`')?;
            Some((name.to_owned(), cells.get(2)?.parse().ok()?))
        })
        .collect()
}

fn reencode<T: BaseCodec>(value: &T) -> Vec<u8> {
    let mut buf = Vec::new();
    value.encode(&mut buf);
    buf
}

/// The evodb key's name (Core's `CDBWrapper` string: a CompactSize length, then the bytes) and
/// the rest of the key.
fn key_name(key: &[u8]) -> (&[u8], &[u8]) {
    let len = usize::from(key[0]);
    (&key[1..1 + len], &key[1 + len..])
}

fn diff_as(diff: &MnListDiff, format: MnListFormat) -> MnListDiff {
    MnListDiff::new(
        format,
        diff.version(),
        diff.base_block_hash(),
        diff.block_hash(),
        diff.cb_tx_merkle_tree().clone(),
        diff.cb_tx().clone(),
        diff.deleted_mns().clone(),
        diff.mn_list().clone(),
        diff.deleted_quorums().clone(),
        diff.new_quorums().clone(),
        diff.quorum_cl_sigs().clone(),
    )
}

/// `info` with every diff written in `format`.
fn qrinfo_as(info: &QrInfo, format: MnListFormat) -> QrInfo {
    QrInfo::new(
        info.snapshot_h_c().clone(),
        info.snapshot_h_2c().clone(),
        info.snapshot_h_3c().clone(),
        diff_as(info.diff_tip(), format),
        diff_as(info.diff_h(), format),
        diff_as(info.diff_h_c(), format),
        diff_as(info.diff_h_2c(), format),
        diff_as(info.diff_h_3c(), format),
        info.cycle_h_4c()
            .as_ref()
            .map(|c| CycleInfo::new(c.snapshot().clone(), diff_as(c.diff(), format))),
        info.last_commitment_per_index().clone(),
        info.quorum_snapshot_list().clone(),
        info.mn_list_diff_list().iter().map(|d| diff_as(d, format)).collect(),
    )
}

/// Every reply decodes as base-sdk's `QrInfo` in its band's layout, whole, and re-encodes to
/// Core's bytes; each `extraShare` request has the H-4C cycle and no other does.
///
/// 70227 (`DMN_TYPE_PROTO_VERSION`) is one-way in Core: its writer puts a list entry's type
/// fields after `isValid` when the entry's own version is BasicBLS or later, but no version, and
/// its reader, with no version to go on, reads the entry as LegacyBLS and stops at `isValid`
/// (`evo/simplifiedmns.h` v24:72-101). base-sdk reads 70227 as Core does, so a 70227 reply is
/// checked through its 70228 sibling instead: the same reply with every entry's version first.
/// Written in 70227's layout, the 70228 decode must give Core's 70227 bytes.
#[test]
fn replies_decode_at_their_band_and_reencode() {
    let fixture = load();
    let replies = fixture.json["replies"].as_array().unwrap();
    let payloads: HashMap<(&str, bool, u64), Vec<u8>> = replies
        .iter()
        .map(|r| {
            let key = (r[0].as_str().unwrap(), r[3].as_bool().unwrap(), r[4].as_u64().unwrap());
            (key, unhex(r[5].as_str().unwrap()))
        })
        .collect();
    for reply in replies {
        let case = reply[0].as_str().unwrap();
        let extra = reply[3].as_bool().unwrap();
        let proto = reply[4].as_u64().unwrap();
        let payload = &payloads[&(case, extra, proto)];
        let format = MnListFormat::for_version(ProtocolVersion::new(proto.try_into().unwrap()));
        let decoded = if proto == 70227 {
            70228
        } else {
            proto
        };
        let decoded_format =
            MnListFormat::for_version(ProtocolVersion::new(decoded.try_into().unwrap()));
        let source = &payloads[&(case, extra, decoded)];
        let mut rest = &source[..];
        let info = QrInfo::decode_as(&mut rest, decoded_format)
            .unwrap_or_else(|e| panic!("{case} {decoded}: {e:?}"));
        assert!(rest.is_empty(), "{case} {decoded}: one payload");
        let info = if decoded == proto {
            info
        } else {
            qrinfo_as(&info, format)
        };
        assert_eq!(reencode(&info), *payload, "{case} {proto}");
        assert_eq!(info.cycle_h_4c().is_some(), extra, "{case} {proto}");
        let block = display_hash(reply[2].as_str().unwrap());
        assert!(fixture.blocks.contains_key(&block), "{case}: the request block is indexed");
        assert_eq!(info.diff_tip().block_hash().as_bytes(), fixture.active.last().unwrap());
    }
}

/// A stale request block below the mining height gets its active sibling's reply, byte for byte.
#[test]
fn stale_answered_equals_active_sibling() {
    let fixture = load();
    let mut by_case = HashMap::new();
    for reply in fixture.json["replies"].as_array().unwrap() {
        let key =
            (reply[0].as_str().unwrap(), reply[3].as_bool().unwrap(), reply[4].as_u64().unwrap());
        by_case.insert(key, reply[5].as_str().unwrap());
    }
    let mut compared = 0;
    for (&(case, extra, proto), payload) in &by_case {
        if let Some(bases) = case.strip_prefix("stale-S-answered/") {
            let sibling = by_case[&(format!("S=before-mining/{bases}").as_str(), extra, proto)];
            assert_eq!(*payload, sibling, "{case} {proto}");
            compared += 1;
        }
    }
    assert_eq!(compared, 36);
}

/// The block index is a tree hanging off the active chain, and every block in it has its data.
#[test]
fn block_index_is_a_tree_of_served_blocks() {
    let fixture = load();
    for (hash, block) in &fixture.blocks {
        assert!(block.have_data, "{}", block.height);
        let Some(prev) = block.prev else {
            assert_eq!(block.height, 0);
            continue;
        };
        assert_eq!(fixture.blocks[&prev].height, block.height - 1);
        if block.status == "active" {
            assert_eq!(fixture.active[block.height as usize], *hash);
        }
    }
}

/// `llmq_S` keys by the indexed block they belong to: `SerializeHash(llmqType, blockHash)`.
fn snapshot_keys(fixture: &Fixture) -> HashMap<Hash256, i32> {
    fixture
        .blocks
        .iter()
        .map(|(hash, block)| (sha256d(&[&[LLMQ_TYPE][..], hash].concat()), block.height))
        .collect()
}

/// The evodb rows decode: every block's masternode list (stale branches' too: Core keeps a
/// disconnected block's `dmn_D4`), each `q_mc` commitment whole and re-encoded, its mined block
/// indexed; the `q_mcih` / `q_mcihi` rows point at an active base block at or below their mining
/// height that has a commitment; every `llmq_S` value is a whole `QuorumSnapshot`, and a key that
/// belongs to an indexed block belongs to a cycle block.
// Core: llmq/blockprocessor.cpp:46-48 (row names), :432-440 (values), :300-309 (index keys);
// llmq/snapshot.cpp:303-318 (`llmq_S`, keyed by SerializeHash(llmqType, blockHash))
#[test]
fn evodb_rows_decode() {
    let fixture = load();
    let rows: Vec<(Vec<u8>, Vec<u8>)> = fixture.json["evodb"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (unhex(r[0].as_str().unwrap()), unhex(r[1].as_str().unwrap())))
        .collect();

    // The source test's `bad-qc-dup` block (`valid-headers`) never connected, so it has no row.
    let diffs: HashSet<&[u8]> = rows
        .iter()
        .map(|(key, _)| key_name(key))
        .filter(|(name, _)| *name == b"dmn_D4")
        .map(|(_, hash)| hash)
        .collect();
    let image = EvoDbImage::from_records(rows.clone()).unwrap();
    for (hash, block) in &fixture.blocks {
        let list = image.list_at(&fixture, hash, DIP3_HEIGHT).unwrap();
        if block.height >= DIP3_HEIGHT && block.status != "valid-headers" {
            assert!(diffs.contains(&hash[..]), "a dmn_D4 row at {}", block.height);
            assert_eq!(list.height(), block.height);
        }
    }

    let snapshot_keys = snapshot_keys(&fixture);
    let mut committed = HashSet::new();
    let mut index_rows = Vec::new();
    for (key, value) in &rows {
        match key_name(key) {
            (b"q_mc", rest) => {
                let mut r = &value[..];
                let qc = Commitment::decode(&mut r).unwrap();
                let mined: Hash256 = r.try_into().expect("the mined block hash follows");
                assert_eq!(reencode(&qc), value[..value.len() - 32]);
                assert_eq!(rest, &value[2..35], "the key is the commitment's type and quorum hash");
                assert!(fixture.blocks.contains_key(&mined));
                committed.insert(rest.to_vec());
            }
            (b"q_mcih", rest) => {
                assert_eq!(rest.len(), 5, "type, inverted height");
                index_rows.push((rest[0], &rest[1..], value));
            }
            (b"q_mcihi", rest) => {
                assert_eq!(rest.len(), 9, "type, quorum index, inverted height");
                assert_eq!(rest[0], LLMQ_TYPE);
                index_rows.push((rest[0], &rest[5..], value));
            }
            (b"llmq_S", rest) => {
                if let Some(height) = snapshot_keys.get(rest) {
                    assert_eq!(height % 24, 0, "a llmq_S row of a cycle block");
                }
                let mut r = &value[..];
                let snapshot = QuorumSnapshot::decode(&mut r).unwrap();
                assert!(r.is_empty());
                assert_eq!(reencode(&snapshot), *value);
            }
            _ => {}
        }
    }
    for (llmq_type, inverted, value) in index_rows {
        let mined = u32::MAX - u32::from_be_bytes(inverted.try_into().unwrap());
        let base = i32::from_le_bytes(value[..].try_into().unwrap());
        assert!(base >= 0 && base as u32 <= mined && (mined as usize) < fixture.active.len());
        let quorum = [&[llmq_type][..], &fixture.active[base as usize]].concat();
        assert!(committed.contains(&quorum), "an index row's base block has a q_mc row");
    }
}

/// The fixture holds what `PROVENANCE.md` says it does.
#[test]
fn counts_match_provenance() {
    let fixture = load();
    let mut counts = BTreeMap::new();
    let mut add = |name: String, n: usize| *counts.entry(name).or_insert(0) += n;
    for reply in fixture.json["replies"].as_array().unwrap() {
        add("replies".into(), 1);
        add(format!("replies {}", reply[4]), 1);
        add(format!("replies {}", reply[0].as_str().unwrap().split('/').next().unwrap()), 1);
    }
    for block in fixture.blocks.values() {
        add("blocks".into(), 1);
        add(format!("blocks {}", block.status), 1);
    }
    let snapshot_keys = snapshot_keys(&fixture);
    for row in fixture.json["evodb"].as_array().unwrap() {
        let key = unhex(row[0].as_str().unwrap());
        let (name, rest) = key_name(&key);
        add(format!("evodb {}", String::from_utf8_lossy(name)), 1);
        if name == b"llmq_S" && snapshot_keys.contains_key(rest) {
            add("evodb llmq_S of an indexed block".into(), 1);
        }
    }
    let refusals =
        std::fs::read_to_string(format!("{}/refusals-feature_llmq_rotation.jsonl", dir())).unwrap();
    for line in refusals.lines() {
        let outcome: Value = serde_json::from_str(line).unwrap();
        assert_eq!(outcome["answered"], Value::Bool(false));
        add("refusals".into(), 1);
    }
    assert_eq!(counts, provenance_counts());
}

/// Each refusal carries the error text, score and bands of `PROVENANCE.md`'s refusals table, and
/// its log line is Core's (`net_processing.cpp` v24.0.0:5696-5700).
#[test]
fn refusals_match_provenance() {
    let fixture = load();
    let at =
        |hex: &str| fixture.blocks.get(&display_hash(hex)).map(|b| (b.height, b.status.as_str()));
    let pair = |text: &str| -> ((i32, &str), (i32, &str)) {
        let (a, b) = text
            .strip_prefix("block ")
            .and_then(|t| t.strip_suffix(" are not in the same chain"))
            .and_then(|t| t.split_once(" and "))
            .unwrap_or_else(|| panic!("not a same-chain error: {text}"));
        (at(a).unwrap(), at(b).unwrap())
    };
    let refusals =
        std::fs::read_to_string(format!("{}/refusals-feature_llmq_rotation.jsonl", dir())).unwrap();
    let mut seen = HashSet::new();
    for line in refusals.lines() {
        let r: Value = serde_json::from_str(line).unwrap();
        let case = r["case"].as_str().unwrap();
        let proto = r["proto"].as_u64().unwrap();
        let expect = r["expect"].as_str().unwrap();
        let block = r["block"].as_str().unwrap();
        let bases: Vec<&str> =
            r["bases"].as_array().unwrap().iter().map(|b| b.as_str().unwrap()).collect();
        assert!(proto == 70242 || proto == 70235, "{case} at {proto}");
        assert!(seen.insert((case.to_owned(), proto)), "{case} at {proto} once");
        assert_eq!(r["answered"], Value::Bool(false), "{case} at {proto}");
        let legacy = proto == 70235;
        match case {
            "unknown-base" => {
                assert_eq!(at(bases[0]), None);
                assert_eq!(expect, format!("block {} not found", bases[0]));
            }
            "unknown-S" => {
                assert_eq!(at(block), None);
                assert_eq!(expect, "block not found");
            }
            "low-S-cycle-base" | "low-S-first-target" => {
                assert_eq!(expect, "Cannot find work block")
            }
            "no-snapshot" => assert_eq!(expect, "Cannot find quorum snapshot"),
            "long-branch-S" if legacy => {
                assert_eq!(at(block), Some((482, "invalid")));
                assert_eq!(pair(expect), ((0, "active"), (472, "invalid")));
            }
            "long-branch-S" => assert_eq!(expect, "Cannot find block"),
            "stale-S-above-mining" => assert_eq!(expect, ""),
            "stale-S-work-above-fork" => {
                let work = if legacy {
                    0
                } else {
                    400
                };
                assert_eq!(pair(expect), ((work, "active"), (424, "valid-fork")));
            }
            "stale-base" => {
                assert_eq!(at(bases[0]).unwrap().1, "valid-fork");
                assert_eq!(expect, format!("block {} is not in the active chain", bases[0]));
            }
            "stale-target-picks-base-above-fork" => {
                assert_eq!(pair(expect), ((423, "active"), (424, "valid-fork")));
                assert_eq!(at(bases[0]), Some((423, "active")));
            }
            _ => panic!("a refusal case PROVENANCE.md doesn't list: {case}"),
        }
        let log: Vec<&str> =
            r["log"].as_array().unwrap().iter().map(|l| l.as_str().unwrap()).collect();
        let tail = format!(
            " (0 -> 1): getquorumrotationinfo failed for size(baseBlockHashes)={}, blockRequestHash={block}. error={expect}",
            bases.len()
        );
        assert_eq!(log.len(), 1, "{case} at {proto}");
        let peer = log[0]
            .strip_prefix("[net] Misbehaving: peer=")
            .and_then(|l| l.strip_suffix(&tail))
            .unwrap_or_else(|| panic!("{case} at {proto}: {}", log[0]));
        assert!(peer.parse::<u32>().is_ok());
    }
    assert_eq!(seen.len(), 20, "10 cases at both bands");
}
