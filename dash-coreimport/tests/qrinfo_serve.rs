//! LQ3c's `qrinfo` fixture against Dash Core v24.0.0: every reply decodes as base-sdk's `QrInfo` at
//! the protocol version it was served at and re-encodes to Core's bytes (70227's through its 70228
//! sibling, see [`replies_decode_at_their_band_and_reencode`]), the evodb rows decode, and the
//! counts are the ones `PROVENANCE.md` states. Then `dash_dml::qrinfo::build_qrinfo`, over
//! [`FixtureSource`] (the fixture's block index and stored evodb rows only), gives every captured
//! reply byte for byte and every captured refusal's text and score (PLAN-LQ3c §4 LQ3c-3).
//!
//! Fixtures: `tests/data/qrinfo/`, built by `contrib/node/lq3c-qrinfo/capture.py` from Core's
//! `feature_llmq_rotation` regtest chain (provenance: `tests/data/qrinfo/PROVENANCE.md`).

use std::cell::RefCell;
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::Read;
use std::sync::Arc;

use dash_consensus::hash::sha256d;
use dash_consensus::primitives::{P2pCommitment, TxRef, decode_tx_exact};
use dash_consensus::{BlockId, ChainAncestry, HeaderCtx};
use dash_coreimport::Hash256;
use dash_coreimport::evodb::{Ancestry, EvoDbImage};
use dash_dml::mnlistdiff::{
    BlockStatus, BlsSignatureBytes, MnListDiffError, MnListDiffRequest, MnListDiffSource,
    PeerOutcome, QuorumBase, QuorumKey, SimplifiedMnListDiff, TargetBlock, build_mnlistdiff,
    check_request,
};
use dash_dml::qrinfo::{
    QrInfoBuildError, QrInfoFailure, QrInfoParams, QrInfoRequest, QrInfoSource, QuorumRotationInfo,
    build_qrinfo,
};
use dash_dml::{MnList, StoreError};
use dash_evo::bls::BlsCtx;
use dash_evo::cbtx::{CoinbaseFacts, nonnull_coinbase_chainlock};
use dash_evo::llmq::{
    MinedCommitment, MinedCommitmentReader, MinedFamily, MinedIndexEntry, base_through,
};
use dash_num::{Arith256, CompactTarget};
use dash_p2p_core::{CycleInfo, MnListDiff, MnListFormat, ProtocolVersion, QrInfo, QuorumSnapshot};
use dash_params::{ChainParamsBuilder, ChainSetup, LlmqParams, LlmqType, Network, Release};
use dash_primitives::{BlockHash, Commitment, QuorumHash};
use dash_types::Numeric;
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

/// A block hash as RPC displays it, from a JSON string.
fn block_hash(value: &Value) -> BlockHash {
    BlockHash::from(display_hash(value.as_str().unwrap()))
}

struct Block {
    height: i32,
    prev: Option<Hash256>,
    have_data: bool,
    status: String,
    coinbase: Vec<u8>,
    txids: Vec<[u8; 32]>,
}

struct Fixture {
    json: Value,
    blocks: HashMap<Hash256, Block>,
    /// Every block, in the fixture's row order.
    order: Vec<Hash256>,
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
    let (mut order, mut active) = (Vec::new(), Vec::new());
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
            coinbase: unhex(row[5].as_str().unwrap()),
            txids: row[6]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| display_hash(t.as_str().unwrap()))
                .collect(),
        };
        assert!(blocks.insert(hash, block).is_none(), "each block once");
        order.push(hash);
    }
    Fixture {
        json,
        blocks,
        order,
        active,
    }
}

fn evodb_records(fixture: &Fixture) -> Vec<(Vec<u8>, Vec<u8>)> {
    fixture.json["evodb"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (unhex(r[0].as_str().unwrap()), unhex(r[1].as_str().unwrap())))
        .collect()
}

fn refusals() -> Vec<Value> {
    std::fs::read_to_string(format!("{}/refusals-feature_llmq_rotation.jsonl", dir()))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// An `llmq_S` key: `SerializeHash(llmqType, blockHash)`.
// Core: v24.0.0 src/llmq/snapshot.cpp:301-318 GetSnapshotForBlock
fn snapshot_key(llmq_type: u8, block: &Hash256) -> Hash256 {
    sha256d(&[&[llmq_type][..], block].concat())
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
        .map(|(hash, block)| (snapshot_key(LLMQ_TYPE, hash), block.height))
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
    let rows = evodb_records(&fixture);

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
    for outcome in refusals() {
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
    let mut seen = HashSet::new();
    for r in &refusals() {
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

/// `EFFICIENT_QRINFO_VERSION`: a peer below it gets the legacy construction.
// Core: v24.0.0 src/version.h:56; src/net_processing.cpp:5693
const EFFICIENT_QRINFO_VERSION: u64 = 70236;

/// The fixture's block index as an arena: [`BlockId`] `i` is the `i`th `blocks` row.
struct Arena {
    headers: Vec<HeaderCtx>,
    prev: Vec<Option<BlockId>>,
    have_data: Vec<bool>,
    ids: HashMap<Hash256, BlockId>,
    /// The active chain by height.
    active: Vec<BlockId>,
}

impl Arena {
    fn new(fixture: &Fixture) -> Arena {
        let hashes = &fixture.order;
        let ids: HashMap<Hash256, BlockId> =
            hashes.iter().enumerate().map(|(i, hash)| (*hash, BlockId(i as u32))).collect();
        let headers = hashes
            .iter()
            .map(|hash| HeaderCtx {
                height: fixture.blocks[hash].height as u32,
                hash: BlockHash::from(*hash),
                time: 0,
                bits: CompactTarget::new(0),
                version: 0,
                mtp: 0,
                chain_work: Arith256::ZERO,
            })
            .collect();
        Arena {
            headers,
            prev: hashes.iter().map(|hash| fixture.blocks[hash].prev.map(|p| ids[&p])).collect(),
            have_data: hashes.iter().map(|hash| fixture.blocks[hash].have_data).collect(),
            active: fixture.active.iter().map(|hash| ids[hash]).collect(),
            ids,
        }
    }

    fn id(&self, hash: &BlockHash) -> Option<BlockId> {
        self.ids.get(hash.as_bytes()).copied()
    }

    fn is_active(&self, id: BlockId) -> bool {
        self.active.get(self.header(id).height as usize) == Some(&id)
    }
}

impl ChainAncestry for Arena {
    fn header(&self, id: BlockId) -> HeaderCtx {
        self.headers[id.0 as usize]
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
        let mut at = id;
        while self.header(at).height > height {
            at = self.prev(at)?;
        }
        (self.header(at).height == height).then_some(at)
    }

    fn prev(&self, id: BlockId) -> Option<BlockId> {
        self.prev[id.0 as usize]
    }
}

/// One row of Core's mined-commitment height indexes: type, quorum index (`q_mcihi`; `None` for
/// `q_mcih`), and the row.
type IndexRow = (u8, Option<i16>, MinedIndexEntry);

/// The fixture's chain: its block index, the evodb rows the builder reads as Core stores them,
/// and regtest's parameters.
struct World {
    fixture: Fixture,
    arena: Arena,
    image: EvoDbImage,
    /// `llmq_S`: `SerializeHash(llmqType, blockHash)` to the stored snapshot.
    snapshots: HashMap<Hash256, Vec<u8>>,
    /// `q_mc`: the stored commitment and the block that mined it.
    commitments: HashMap<QuorumKey, (Vec<u8>, Hash256)>,
    index: Vec<IndexRow>,
    setup: ChainSetup,
    dip0024_height: u32,
    v20_height: i32,
}

impl World {
    fn load() -> World {
        let fixture = load();
        let arena = Arena::new(&fixture);
        let records = evodb_records(&fixture);
        let (mut snapshots, mut commitments, mut index) = (HashMap::new(), HashMap::new(), vec![]);
        let entry = |inverted_height: &[u8], value: &[u8]| MinedIndexEntry {
            mined_height: u32::MAX - u32::from_be_bytes(inverted_height.try_into().unwrap()),
            base_height: u32::try_from(i32::from_le_bytes(value.try_into().unwrap())).unwrap(),
        };
        // Core: llmq/blockprocessor.cpp:46-48 (row names), :300-309 (index keys); llmq/snapshot.cpp:303-318
        for (key, value) in &records {
            match key_name(key) {
                (b"llmq_S", rest) => {
                    snapshots.insert(rest.try_into().unwrap(), value.clone());
                }
                (b"q_mc", rest) => {
                    let (qc, mined) = value.split_at(value.len() - 32);
                    let quorum = (rest[0], rest[1..].try_into().unwrap());
                    commitments.insert(quorum, (qc.to_vec(), mined.try_into().unwrap()));
                }
                (b"q_mcih", rest) => {
                    index.push((rest[0], None, entry(&rest[1..], value)));
                }
                (b"q_mcihi", rest) => {
                    let quorum_index = i32::from_le_bytes(rest[1..5].try_into().unwrap());
                    let quorum_index = Some(i16::try_from(quorum_index).unwrap());
                    index.push((rest[0], quorum_index, entry(&rest[5..], value)));
                }
                _ => {}
            }
        }
        // The reader's premise: Core's index holds only the active chain's rows, each naming an
        // active base whose `q_mc` row the active block at its mining height mined.
        for &(llmq_type, quorum_index, row) in &index {
            let base = (llmq_type, fixture.active[row.base_height as usize]);
            let (qc, mined) = &commitments[&base];
            assert_eq!(*mined, fixture.active[row.mined_height as usize]);
            assert_eq!(quorum_index.unwrap_or(0), commitment_quorum_index(qc));
        }
        for (_, mined) in commitments.values() {
            let id = arena.id(&BlockHash::from(*mined)).unwrap();
            assert_eq!(*mined, fixture.active[arena.header(id).height as usize]);
        }
        let deployments = &fixture.json["deployments"];
        let setup = ChainParamsBuilder::new(Network::Regtest, Release::V24_0)
            .build()
            .expect("Core's regtest parameters");
        World {
            image: EvoDbImage::from_records(records).unwrap(),
            arena,
            snapshots,
            commitments,
            index,
            setup,
            dip0024_height: deployments["dip0024"].as_u64().unwrap() as u32,
            v20_height: deployments["v20"].as_i64().unwrap() as i32,
            fixture,
        }
    }

    fn params(&self) -> QrInfoParams<'_> {
        let llmq_type = self.setup.consensus.llmq_type_dip0024_instant_send;
        let params = self.setup.consensus.llmq(llmq_type).expect("configured");
        assert_eq!(llmq_type.to_base(), LLMQ_TYPE);
        assert_eq!(
            Some(u64::from(params.dkg_interval)),
            self.fixture.json["dkg_interval"].as_u64()
        );
        let genesis_hash = BlockHash::from(*self.setup.consensus.hash_genesis_block.as_bytes());
        assert_eq!(genesis_hash, self.arena.header(self.arena.active[0]).hash);
        QrInfoParams {
            llmq_type,
            params,
            genesis_hash,
        }
    }

    fn source(&self) -> FixtureSource<'_> {
        FixtureSource {
            world: self,
            lists: RefCell::new(HashMap::new()),
        }
    }

    /// `IsQuorumRotationEnabled(params, at)`: DIP0024 active after the block before `at`'s cycle.
    // Core: v24.0.0 src/llmq/options.cpp:50-62
    fn rotation_enabled(&self, params: &LlmqParams, at: BlockId) -> bool {
        let height = self.arena.header(at).height;
        let cycle_base = height - height % u32::from(params.dkg_interval);
        params.use_rotation && cycle_base >= 1 && cycle_base >= self.dip0024_height
    }
}

/// The active chain's mined-commitment index over the stored `q_mc`, `q_mcih` and `q_mcihi` rows.
/// Core's index holds only the active chain's rows (a disconnect erases a block's,
/// `llmq/blockprocessor.cpp` v24.0.0:536-547), and Core reads it at the tip. For any other
/// `chain_end` a row counts only when `chain_end`'s ancestor at its mining height is the active
/// block there: `MinedCommitmentReader::mined_index`'s contract (rows whose mining block is
/// `chain_end` or one of its ancestors), as `MemEvoView::mined_index` keeps it. That filter is
/// what the stale request blocks exercise: a builder reading the index through `S`'s ancestry
/// instead of `(T, h(S))` (PLAN-LQ3c mutant 8) sees other rows than Core and fails
/// [`builder_refuses_each_captured_request_as_core_logs_it`].
impl MinedCommitmentReader for World {
    type Error = StoreError;

    fn chain(&self) -> &dyn ChainAncestry {
        &self.arena
    }

    fn mined_commitment(
        &self,
        llmq_type: LlmqType,
        quorum_hash: &QuorumHash,
        at: BlockId,
    ) -> Result<Option<MinedCommitment>, StoreError> {
        let arena = &self.arena;
        let quorum = (llmq_type.to_base(), *quorum_hash.as_bytes());
        let Some((qc, mined)) = self.commitments.get(&quorum) else {
            return Ok(None);
        };
        let mined_id = arena.id(&BlockHash::from(*mined)).ok_or(StoreError::read("q_mc block"))?;
        if arena.ancestor(at, arena.header(mined_id).height) != Some(mined_id) {
            return Ok(None);
        }
        let qc =
            Commitment::decode(&mut &qc[..]).map_err(|e| StoreError::read(format!("{e:?}")))?;
        Ok(Some((Arc::new(P2pCommitment::new(qc)), BlockHash::from(*mined))))
    }

    fn mined_index(
        &self,
        llmq_type: LlmqType,
        family: MinedFamily,
        chain_end: BlockId,
        max_mined_height: u32,
        limit: usize,
    ) -> Result<Vec<MinedIndexEntry>, StoreError> {
        let arena = &self.arena;
        let quorum_index = match family {
            MinedFamily::NonRotated => None,
            MinedFamily::Rotated {
                quorum_index,
            } => Some(quorum_index),
        };
        let cap = max_mined_height.min(arena.header(chain_end).height);
        let mut rows: Vec<MinedIndexEntry> = self
            .index
            .iter()
            .filter(|row| (row.0, row.1) == (llmq_type.to_base(), quorum_index))
            .map(|row| row.2)
            .filter(|row| (1..=cap).contains(&row.mined_height))
            .filter(|row| {
                let active = arena.active.get(row.mined_height as usize);
                arena.ancestor(chain_end, row.mined_height).is_some_and(|a| active == Some(&a))
            })
            .collect();
        rows.sort_by_key(|row| Reverse(row.mined_height));
        rows.truncate(limit);
        Ok(rows)
    }
}

/// `BuildQuorumRotationInfo`'s reads over the fixture (PLAN-LQ3c §2): the block index, the stored
/// `llmq_S` rows, the [`World`] mined-commitment reader, and each inner diff built as EV4's `ReplySource` builds it
/// (`evodb_serve.rs`), non-extended, its quorum sets computed from the index rows. Nothing is
/// taken from Core's replies, and no row is computed or rebuilt (DEC-1792 (a)).
struct FixtureSource<'a> {
    world: &'a World,
    lists: RefCell<HashMap<BlockHash, MnList>>,
}

impl FixtureSource<'_> {
    fn status(&self, hash: &BlockHash) -> Option<BlockStatus> {
        let arena = &self.world.arena;
        arena.id(hash).map(|id| BlockStatus {
            height: arena.header(id).height as i32,
            in_active_chain: arena.is_active(id),
            have_data: self.have_data(id),
        })
    }

    fn list(&self, block: &BlockHash) -> Result<MnList, StoreError> {
        if let Some(list) = self.lists.borrow().get(block) {
            return Ok(list.clone());
        }
        let list = self
            .world
            .image
            .list_at(&self.world.fixture, block.as_bytes(), DIP3_HEIGHT)
            .map_err(|e| StoreError::read(format!("{e:?}")))?;
        self.lists.borrow_mut().insert(*block, list.clone());
        Ok(list)
    }
}

impl QrInfoSource for FixtureSource<'_> {
    type Error = StoreError;
    type Diff = SimplifiedMnListDiff;

    fn lookup(&self, hash: &BlockHash) -> Option<BlockId> {
        self.world.arena.id(hash)
    }

    fn active_genesis(&self) -> Option<BlockId> {
        self.world.arena.active.first().copied()
    }

    fn active_tip(&self) -> Option<BlockId> {
        self.world.arena.active.last().copied()
    }

    fn in_active_chain(&self, id: BlockId) -> bool {
        self.world.arena.is_active(id)
    }

    fn have_data(&self, id: BlockId) -> bool {
        self.world.arena.have_data[id.0 as usize]
    }

    // Core: v24.0.0 src/llmq/snapshot.cpp:301-318 GetSnapshotForBlock
    fn quorum_snapshot(
        &self,
        llmq_type: LlmqType,
        cycle: &BlockHash,
    ) -> Result<Option<Arc<dash_evo::llmq::QuorumSnapshot>>, StoreError> {
        let key = snapshot_key(llmq_type.to_base(), cycle.as_bytes());
        Ok(self.world.snapshots.get(&key).map(|row| Arc::new(snapshot_row(row))))
    }

    fn mined(&self) -> &dyn MinedCommitmentReader<Error = StoreError> {
        self.world
    }

    fn commitment(&self, quorum: &QuorumKey) -> Result<Option<Vec<u8>>, StoreError> {
        let (llmq_type, hash) = (LlmqType::from_base(quorum.0), QuorumHash::from(quorum.1));
        let mined = self.world.mined_commitment(llmq_type, &hash, self.active_tip().unwrap())?;
        Ok(mined.map(|_| self.world.commitments[quorum].0.clone()))
    }

    fn mnlistdiff(
        &self,
        base: &BlockHash,
        block: &BlockHash,
    ) -> Result<SimplifiedMnListDiff, MnListDiffError<StoreError>> {
        let target = self.status(block);
        check_request(base, self.status(base), block, target)?;
        let (base_list, list) = (
            self.list(base).map_err(MnListDiffError::Source)?,
            self.list(block).map_err(MnListDiffError::Source)?,
        );
        let request = MnListDiffRequest {
            requested_base: *base,
            base_block: *base,
            base_list: &base_list,
            block: *block,
            list: &list,
            v20_active_after: target.unwrap().height + 1 >= self.world.v20_height,
        };
        build_mnlistdiff(&request, self)
    }
}

impl MnListDiffSource for FixtureSource<'_> {
    type Error = StoreError;

    /// `GetMinedAndActiveCommitmentsUntilBlock(block)`, from the index rows.
    // Core: v24.0.0 src/llmq/blockprocessor.cpp:818-830
    fn active_quorums(&self, block: &BlockHash) -> Result<BTreeSet<QuorumKey>, StoreError> {
        let arena = &self.world.arena;
        let at = arena.id(block).ok_or(StoreError::read("block not in the index"))?;
        let height = arena.header(at).height;
        let mut out = BTreeSet::new();
        for (llmq_type, params) in self.world.setup.consensus.llmq_params() {
            let count = usize::from(params.signing_active_quorum_count);
            let rows = if self.world.rotation_enabled(params, at) {
                let mut rows = Vec::new();
                for quorum_index in 0..count {
                    let family = MinedFamily::Rotated {
                        quorum_index: i16::try_from(quorum_index).unwrap(),
                    };
                    rows.extend(self.world.mined_index(llmq_type, family, at, height, 1)?);
                }
                rows
            } else {
                self.world.mined_index(llmq_type, MinedFamily::NonRotated, at, height, count)?
            };
            for row in rows {
                let base = base_through::<StoreError>(arena, at, row.base_height)?;
                out.insert((llmq_type.to_base(), *arena.header(base).hash.as_bytes()));
            }
        }
        Ok(out)
    }

    fn mined_commitment(&self, quorum: &QuorumKey) -> Result<Option<Vec<u8>>, StoreError> {
        self.commitment(quorum)
    }

    fn quorum(&self, quorum: &QuorumKey) -> Result<Option<QuorumBase>, StoreError> {
        let arena = &self.world.arena;
        let (Some(base), Some(commitment)) =
            (arena.id(&BlockHash::from(quorum.1)), self.commitment(quorum)?)
        else {
            return Ok(None);
        };
        Ok(Some(QuorumBase {
            base_height: arena.header(base).height as i32,
            quorum_index: commitment_quorum_index(&commitment),
        }))
    }

    fn coinbase_chainlock(
        &self,
        block: &BlockHash,
        height: i32,
    ) -> Result<Option<BlsSignatureBytes>, StoreError> {
        let arena = &self.world.arena;
        let Ok(height) = u32::try_from(height) else {
            return Ok(None);
        };
        let at = arena.id(block).ok_or(StoreError::read("block not in the index"))?;
        let Some(work) = arena.ancestor(at, height) else {
            return Ok(None);
        };
        let coinbase = &self.world.fixture.blocks[arena.header(work).hash.as_bytes()].coinbase;
        let facts: TxRef = Arc::new(decode_tx_exact(coinbase).unwrap().into_value());
        let facts = CoinbaseFacts::First(facts);
        let v20 = height as i32 >= self.world.v20_height;
        Ok(nonnull_coinbase_chainlock(&facts, height, v20, BlsCtx::BASIC).map(|(sig, _)| {
            let bytes: Vec<u8> = sig.to_bytes(BlsCtx::BASIC.scheme()).into();
            bytes.try_into().unwrap()
        }))
    }

    fn target_block(&self, block: &BlockHash) -> Option<TargetBlock> {
        let block = &self.world.fixture.blocks[block.as_bytes()];
        Some(TargetBlock {
            coinbase: block.coinbase.clone(),
            txids: block.txids.clone(),
        })
    }
}

/// A serialized `CFinalCommitment`'s `quorumIndex`: after nVersion, llmqType and quorumHash in
/// the indexed versions 2 and 4, 0 otherwise (`llmq/commitment.h` v24.0.0:95-106).
fn commitment_quorum_index(commitment: &[u8]) -> i16 {
    match u16::from_le_bytes([commitment[0], commitment[1]]) {
        2 | 4 => i16::from_le_bytes([commitment[35], commitment[36]]),
        _ => 0,
    }
}

fn read_compact(bytes: &[u8], at: &mut usize) -> usize {
    let first = bytes[*at];
    *at += 1;
    let width = match first {
        0xfd => 2,
        0xfe => 4,
        0xff => 8,
        n => return usize::from(n),
    };
    let mut value = [0; 8];
    value[..width].copy_from_slice(&bytes[*at..*at + width]);
    *at += width;
    u64::from_le_bytes(value) as usize
}

/// A stored `CQuorumSnapshot`: the skip mode, the member bits, the skip list, and nothing after.
// Core: v24.0.0 src/llmq/snapshot.h:60-71 CQuorumSnapshot::Serialize
fn snapshot_row(row: &[u8]) -> dash_evo::llmq::QuorumSnapshot {
    let skip_list_mode = i32::from_le_bytes(row[..4].try_into().unwrap());
    let mut at = 4;
    let members = read_compact(row, &mut at);
    let active_quorum_members = (0..members).map(|i| row[at + i / 8] >> (i % 8) & 1 == 1).collect();
    at += members.div_ceil(8);
    let skips = read_compact(row, &mut at);
    let skip_list = (0..skips)
        .map(|i| i32::from_le_bytes(row[at + 4 * i..at + 4 * i + 4].try_into().unwrap()))
        .collect();
    assert_eq!(at + 4 * skips, row.len(), "one snapshot per row");
    dash_evo::llmq::QuorumSnapshot {
        active_quorum_members,
        skip_list_mode,
        skip_list,
    }
}

type Built = Result<
    QuorumRotationInfo<SimplifiedMnListDiff>,
    QrInfoFailure<SimplifiedMnListDiff, StoreError>,
>;

/// `getqrinfo` as Core serves it to a peer at `proto`: the legacy construction below
/// [`EFFICIENT_QRINFO_VERSION`] (`net_processing.cpp` v24.0.0:5693).
fn serve(
    src: &FixtureSource<'_>,
    bases: &[BlockHash],
    block: &BlockHash,
    extra: bool,
    proto: u64,
) -> Built {
    let request = QrInfoRequest {
        base_block_hashes: bases,
        block_request_hash: *block,
        extra_share: extra,
    };
    build_qrinfo(src, &src.world.params(), &request, proto < EFFICIENT_QRINFO_VERSION)
}

/// A captured reply: case, bases, request block, `extraShare`, protocol version, payload.
type Reply = (String, Vec<BlockHash>, BlockHash, bool, u64, Vec<u8>);

fn captured_replies(fixture: &Fixture) -> Vec<Reply> {
    fixture.json["replies"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r[0].as_str().unwrap().to_owned(),
                r[1].as_array().unwrap().iter().map(block_hash).collect(),
                block_hash(&r[2]),
                r[3].as_bool().unwrap(),
                r[4].as_u64().unwrap(),
                unhex(r[5].as_str().unwrap()),
            )
        })
        .collect()
}

/// Every captured reply, rebuilt by `build_qrinfo` over [`FixtureSource`] at its band (legacy
/// below 70236) and encoded at that band, equals Core's bytes.
///
/// 70227 is compared directly: Core's 70227 writer and dash-dml's encoder at 70227 both put a
/// BasicBLS entry's type fields after `isValid` without the entry's version
/// (`evo/simplifiedmns.h` v24:72-101), so the builder's 70227 bytes are already in 70227's
/// layout. That is the layout [`replies_decode_at_their_band_and_reencode`] compares in, after
/// it decodes the 70228 sibling, since a 70227 reply can't be read back.
#[test]
fn builder_matches_every_captured_reply() {
    let world = World::load();
    let src = world.source();
    let mut matched: BTreeMap<u64, usize> = BTreeMap::new();
    let mut differ = Vec::new();
    for (case, bases, block, extra, proto, payload) in captured_replies(&world.fixture) {
        let what = format!("{case} extraShare={extra} at {proto}");
        let info = serve(&src, &bases, &block, extra, proto)
            .unwrap_or_else(|failure| panic!("{what}: refused: {:?}", failure.error));
        let bytes = info.to_bytes(proto as i32);
        if bytes == payload {
            *matched.entry(proto).or_default() += 1;
        } else {
            let first = bytes.iter().zip(&payload).position(|(a, b)| a != b);
            let first = first.unwrap_or(bytes.len().min(payload.len()));
            differ.push(format!(
                "{what}: first differing byte {first} ({} vs Core's {} bytes)",
                bytes.len(),
                payload.len()
            ));
        }
    }
    assert!(differ.is_empty(), "{}", differ.join("\n"));
    let bands = [70221, 70225, 70227, 70228, 70229, 70230, 70235, 70236, 70242];
    assert_eq!(matched, bands.into_iter().map(|band| (band, 52)).collect());
}

/// Each captured refusal: `build_qrinfo` refuses the same request at the same band, its error's
/// `Display` is Core's `errorRet` and its `peer_outcome()` Core's score, and the two compose the
/// captured `debug.log` line as `net_processing.cpp` v24.0.0:5697-5701 does (`Misbehaving(1)`
/// logs `(0 -> 1)` for a fresh peer).
///
/// The stale request blocks here are where the index read differs by chain: `stale-S-above-mining`
/// gets Core's empty error only if the rows are read at `(T, cap h(S))` and their bases mapped
/// through `S` (the [`World`] reader's ancestry filter is the trait's contract that makes a read through
/// `S`'s ancestry visibly different).
#[test]
fn builder_refuses_each_captured_request_as_core_logs_it() {
    let world = World::load();
    let src = world.source();
    let mut refused = 0;
    for r in refusals() {
        let case = r["case"].as_str().unwrap();
        let proto = r["proto"].as_u64().unwrap();
        let bases: Vec<BlockHash> = r["bases"].as_array().unwrap().iter().map(block_hash).collect();
        let block = block_hash(&r["block"]);
        let what = format!("{case} at {proto}");
        let error = match serve(&src, &bases, &block, r["extra"].as_bool().unwrap(), proto) {
            Ok(_) => panic!("{what}: answered"),
            Err(failure) => failure.error,
        };
        assert_eq!(error.to_string(), r["expect"].as_str().unwrap(), "{what}: {error:?}");
        if case == "stale-S-above-mining" {
            assert!(matches!(error, QrInfoBuildError::NoMinedCommitment(_)), "{what}: {error:?}");
        }
        let PeerOutcome::Misbehaving(score) = error.peer_outcome() else {
            panic!("{what}: unscored")
        };
        let [log] = &r["log"].as_array().unwrap()[..] else {
            panic!("{what}: one log line")
        };
        let log = log.as_str().unwrap();
        let peer = log
            .strip_prefix("[net] Misbehaving: peer=")
            .and_then(|l| l.split_once(' '))
            .unwrap_or_else(|| panic!("{what}: {log}"))
            .0;
        let composed = format!(
            "[net] Misbehaving: peer={peer} (0 -> {score}): getquorumrotationinfo failed for size(baseBlockHashes)={}, blockRequestHash={}. error={error}",
            bases.len(),
            r["block"].as_str().unwrap()
        );
        assert_eq!(composed, log, "{what}");
        refused += 1;
    }
    assert_eq!(refused, 20);
}

/// The answered stale request block (branch A's block at c+2, below the mining height) gets the
/// builder's reply for its active sibling, byte for byte, as Core gave it: the cycle base and H
/// come from `S`'s ancestry, but everything after from the tip's, and the index read is capped at
/// `h(S)` (PLAN-LQ3c mutant 18: no active-chain check on `S`). The other tests imply it (Core's
/// two payloads are equal and the builder gives each); this one states it of the builder directly.
#[test]
fn stale_answered_builds_its_active_siblings_reply() {
    let world = World::load();
    let src = world.source();
    let replies = captured_replies(&world.fixture);
    let siblings: HashMap<(&str, bool, u64), &Reply> =
        replies.iter().map(|r| ((r.0.as_str(), r.3, r.4), r)).collect();
    let mut compared = 0;
    for (case, bases, block, extra, proto, payload) in &replies {
        let Some(base_set) = case.strip_prefix("stale-S-answered/") else {
            continue;
        };
        let sibling = siblings[&(format!("S=before-mining/{base_set}").as_str(), *extra, *proto)];
        assert_eq!((&sibling.1, sibling.2 == *block), (bases, false));
        assert!(!world.arena.is_active(world.arena.id(block).unwrap()), "{case}: a stale block");
        let stale = serve(&src, bases, block, *extra, *proto).unwrap().to_bytes(*proto as i32);
        let active =
            serve(&src, &sibling.1, &sibling.2, *extra, *proto).unwrap().to_bytes(*proto as i32);
        assert_eq!(stale, active, "{case} at {proto}");
        assert_eq!(stale, *payload, "{case} at {proto}");
        compared += 1;
    }
    assert_eq!(compared, 36);
}
