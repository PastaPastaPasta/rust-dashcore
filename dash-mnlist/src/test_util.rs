//! Test-fixture replay shared with downstream bridge tests.

use std::collections::HashMap;
use std::io::Read;
use std::sync::{Arc, OnceLock};

use dash_chain::{AncestryError, AnchoredAncestry, BlockRef};
use dash_consensus::BlockId;
use dash_consensus::ctx::HeaderCtx;
use dash_consensus::primitives::{P2pMnListDiff, P2pQrInfo};
use dash_consensus::primitives::{decode_header, decode_tx_exact};
#[cfg(test)]
use dash_dml::codec::core_disk::decode_list_s3;
use dash_evo::cbtx::CoinbaseFacts;
use dash_evo::llmq::QuorumSnapshot;
use dash_evo::sml::{SmlEntry, SmlList};
use dash_num::{Arith256, CompactTarget};
use dash_p2p_core::ClSig;
use dash_p2p_core::QuorumSnapshot as WireSnapshot;
use dash_params::{ChainParamsBuilder, LlmqType, Network, Release};
use dash_primitives::{BlockHash, Commitment, MerkleRoot, QuorumHash};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;
use serde_json::Value;

use crate::convert::snapshot;

use crate::{
    LlmqConfig, MasternodeListEngine, MnChain, MnChange, MnReject, MnRequest, Mode, QuorumState,
    RequestKey,
};

/// A capture's headers, height as id, from its first height.
pub(crate) struct CaptureChain {
    first: u32,
    headers: Vec<HeaderCtx>,
    ids: HashMap<BlockHash, u32>,
}

impl CaptureChain {
    pub(crate) fn new(first: u32, raw: &[u8]) -> Self {
        let headers: Vec<HeaderCtx> = raw
            .chunks(80)
            .zip(first..)
            .map(|(h, height)| {
                let header = decode_header(&mut &h[..]).expect("a header").into_value();
                HeaderCtx {
                    height,
                    hash: header.hash(),
                    time: header.raw().time(),
                    bits: CompactTarget::new(header.raw().bits()),
                    version: header.raw().version(),
                    mtp: 0,
                    chain_work: Arith256::ZERO,
                }
            })
            .collect();
        let ids = headers.iter().map(|h| (h.hash, h.height)).collect();
        CaptureChain {
            first,
            headers,
            ids,
        }
    }

    fn tip(&self) -> u32 {
        self.first + u32::try_from(self.headers.len()).expect("small") - 1
    }

    pub(crate) fn hash(&self, height: u32) -> BlockHash {
        self.headers[(height - self.first) as usize].hash
    }

    fn held(&self, height: u32) -> Result<usize, AncestryError> {
        if height < self.first {
            return Err(AncestryError::BelowFloor {
                height,
            });
        }
        assert!(height <= self.tip());
        Ok((height - self.first) as usize)
    }
}

impl AnchoredAncestry for CaptureChain {
    fn floor(&self) -> u32 {
        self.first
    }

    fn header(&self, id: BlockId) -> Result<HeaderCtx, AncestryError> {
        Ok(self.headers[self.held(id.0)?])
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Result<Option<BlockId>, AncestryError> {
        if height > id.0 {
            return Ok(None);
        }
        self.held(height).map(|_| Some(BlockId(height)))
    }

    fn prev(&self, id: BlockId) -> Result<Option<BlockId>, AncestryError> {
        match id.0.checked_sub(1) {
            None => Ok(None),
            Some(h) => self.held(h).map(|_| Some(BlockId(h))),
        }
    }
}

impl MnChain for CaptureChain {
    fn ancestry(&self) -> &dyn AnchoredAncestry {
        self
    }

    fn lookup(&self, hash: &BlockHash) -> Result<Option<BlockId>, AncestryError> {
        Ok(self.ids.get(hash).map(|h| BlockId(*h)))
    }

    fn active_tip(&self) -> Result<BlockRef, AncestryError> {
        let tip = self.tip();
        Ok(BlockRef {
            id: BlockId(tip),
            hash: self.hash(tip),
            height: tip,
        })
    }

    fn merkle_root(&self, _: BlockId) -> Result<Option<MerkleRoot>, AncestryError> {
        unreachable!("step 5 reads no merkle root")
    }
}

/// Decodes one evodb list into its SML entries, in list order. The caller supplies it because
/// layer rule L2 bars a normal dependency on dash-dml.
pub type ListDecoder = fn(&[u8]) -> Vec<SmlEntry>;

/// dash-dml's decoder: each masternode's `Dmn::to_sml_entry` (Core's
/// `CDeterministicMN::to_sml_entry`).
#[cfg(test)]
pub(crate) fn dml_lists(bytes: &[u8]) -> Vec<SmlEntry> {
    let list = decode_list_s3(&mut &bytes[..]).expect("an evodb list");
    list.iter().map(|d| d.to_sml_entry()).collect()
}

fn row(hex_row: &Value) -> QuorumSnapshot {
    let bytes = hex(hex_row.as_str().expect("a row"));
    let mut r = &bytes[..];
    let wire = WireSnapshot::decode(&mut r).expect("Core's row");
    assert!(r.is_empty());
    snapshot(&wire)
}

/// Two operator keys swapped in every list (a rotated member may come from an earlier quarter's).
type Swap = ([u8; 48], [u8; 48]);

/// A capture's lists, coinbases and rows held by an engine, as the trust chain would hold them.
pub(crate) fn held(
    v: &Value,
    chain: &CaptureChain,
    cfg: &LlmqConfig,
    swap: Option<Swap>,
    decode: ListDecoder,
) -> MasternodeListEngine {
    let mut engine = MasternodeListEngine::new(Mode::Client);
    let dmns: Vec<Vec<u8>> = v["dmns"]
        .as_array()
        .expect("records")
        .iter()
        .map(|d| hex(d.as_str().expect("hex")))
        .collect();
    let coinbases: HashMap<u32, CoinbaseFacts> = v["coinbases"]
        .as_array()
        .expect("coinbases")
        .iter()
        .map(|cb| {
            let tx = decode_tx_exact(&hex(cb["tx"].as_str().expect("a coinbase")))
                .expect("a coinbase")
                .into_value();
            (u32_of(&cb["height"]), CoinbaseFacts::First(Arc::new(tx)))
        })
        .collect();
    for list in v["lists"].as_array().expect("lists") {
        let height = u32_of(&list["height"]);
        let entries = list["entries"].as_array().expect("entries");
        let mut bytes = hex(list["head"].as_str().expect("a head"));
        dash_consensus::primitives::write_compact_size(entries.len() as u64, &mut bytes);
        for e in entries {
            bytes.extend(&dmns[usize::try_from(e.as_u64().expect("an index")).expect("small")]);
        }
        let mut sml = decode(&bytes);
        if let Some((a, b)) = swap {
            for e in &mut sml {
                if e.operator_key == a {
                    e.operator_key = b;
                } else if e.operator_key == b {
                    e.operator_key = a;
                }
            }
        }
        let coinbase =
            coinbases.get(&height).cloned().unwrap_or(CoinbaseFacts::BlockDataUnavailable);
        engine.hold(
            BlockId(height),
            height,
            chain.hash(height),
            SmlList::from_entries(sml),
            coinbase,
        );
    }
    let rotated = cfg.params().consensus.llmq_type_dip0024_instant_send;
    for r in v["rows"].as_array().expect("rows") {
        engine.hold_row(rotated, chain.hash(u32_of(&r["cycle"])), row(&r["snapshot"]));
    }
    engine
}

/// The testnet capture's headers from its floor, with their merkle roots, height as id. Headers
/// above `tip` are not yet accepted. It has one branch: the tip only rises, so a reorg or a lowered
/// tip is built on a branching chain, not on this one (D63).
pub struct RigChain {
    floor: u32,
    headers: Vec<HeaderCtx>,
    roots: Vec<MerkleRoot>,
    ids: HashMap<BlockHash, u32>,
    tip: u32,
}

impl RigChain {
    fn new(v: &Value, tip: u32) -> Self {
        let floor = u32_of(&v["floor"]);
        let mut headers: Vec<HeaderCtx> = Vec::new();
        let mut roots = Vec::new();
        let mut times = Vec::new();
        for (height, h) in (floor..).zip(v["headers"].as_array().expect("headers")) {
            let header = decode_header(&mut &hex(h.as_str().expect("a header"))[..])
                .expect("a header")
                .into_value();
            let raw = header.raw();
            if let Some(prev) = headers.last() {
                assert_eq!(raw.prev_hash(), prev.hash, "the headers chain");
            }
            times.push(i64::from(raw.time()));
            let mut window: Vec<i64> = times.iter().rev().take(11).copied().collect();
            window.sort_unstable();
            roots.push(raw.merkle_root());
            headers.push(HeaderCtx {
                height,
                hash: header.hash(),
                time: raw.time(),
                bits: CompactTarget::new(raw.bits()),
                version: raw.version(),
                mtp: window[window.len() / 2],
                chain_work: Arith256::ZERO,
            });
        }
        let ids = headers.iter().map(|h| (h.hash, h.height)).collect();
        let mut chain = RigChain {
            floor,
            headers,
            roots,
            ids,
            tip: floor,
        };
        chain.set_tip(tip);
        chain
    }

    /// The active tip's height.
    pub fn tip(&self) -> u32 {
        self.tip
    }

    /// The highest header the capture holds.
    pub fn captured_tip(&self) -> u32 {
        self.floor + u32::try_from(self.headers.len()).expect("small") - 1
    }

    /// Accepts the capture's headers up to `tip`.
    ///
    /// # Panics
    /// Panics if `tip` is outside the capture or below the tip: the chain has no branch.
    pub fn set_tip(&mut self, tip: u32) {
        assert!((self.floor..=self.captured_tip()).contains(&tip), "a captured header");
        assert!(
            tip >= self.tip,
            "the rig's chain has no branch: build a reorg or a lowered tip on a branching chain \
             such as the regtest tests' `FixtureChain`"
        );
        self.tip = tip;
    }

    /// The hash of the block at `height`.
    pub fn hash(&self, height: u32) -> BlockHash {
        self.headers[(height - self.floor) as usize].hash
    }

    /// The height of an accepted block.
    pub fn height(&self, hash: &BlockHash) -> Option<u32> {
        self.ids.get(hash).copied().filter(|h| *h <= self.tip)
    }

    fn index(&self, height: u32) -> Result<usize, AncestryError> {
        if height < self.floor {
            return Err(AncestryError::BelowFloor {
                height,
            });
        }
        assert!(height <= self.tip, "an id the chain never issued");
        Ok((height - self.floor) as usize)
    }
}

impl AnchoredAncestry for RigChain {
    fn floor(&self) -> u32 {
        self.floor
    }

    fn header(&self, id: BlockId) -> Result<HeaderCtx, AncestryError> {
        Ok(self.headers[self.index(id.0)?])
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Result<Option<BlockId>, AncestryError> {
        if height > id.0 {
            return Ok(None);
        }
        self.index(height).map(|_| Some(BlockId(height)))
    }

    fn prev(&self, id: BlockId) -> Result<Option<BlockId>, AncestryError> {
        match id.0.checked_sub(1) {
            None => Ok(None),
            Some(h) => self.index(h).map(|_| Some(BlockId(h))),
        }
    }
}

impl MnChain for RigChain {
    fn ancestry(&self) -> &dyn AnchoredAncestry {
        self
    }

    fn lookup(&self, hash: &BlockHash) -> Result<Option<BlockId>, AncestryError> {
        Ok(self.height(hash).map(BlockId))
    }

    fn active_tip(&self) -> Result<BlockRef, AncestryError> {
        Ok(BlockRef {
            id: BlockId(self.tip),
            hash: self.hash(self.tip),
            height: self.tip,
        })
    }

    fn merkle_root(&self, id: BlockId) -> Result<Option<MerkleRoot>, AncestryError> {
        Ok(Some(self.roots[self.index(id.0)?]))
    }
}

/// Core's reply to one of the engine's requests.
#[derive(Clone, Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "returned and applied at once: a box would allocate per reply"
)]
pub enum Reply {
    /// An `mnlistdiff`.
    Diff(P2pMnListDiff),
    /// A `qrinfo`.
    QrInfo(P2pQrInfo),
}

/// A host-test rig over the real engine (SB15 S2a, D63): the testnet capture's chain with its
/// merkle roots and testnet's `LlmqConfig`. It answers the engine's requests with Core's replies
/// from the testnet capture (step diffs, the diff from genesis and the `qrinfo`s) and its fetch
/// capture, through `apply_diff` and `apply_qrinfo`; the host gives up on the rest with
/// `request_failed`. A `getmnlistd` is answered with Core's reply to exactly that request, and only
/// if a capture holds it: the captures hold no diff between arbitrary blocks, so a request whose
/// base is not its target's parent stays unanswered unless the fetch capture holds it. A
/// `getqrinfo` is matched by its height only: it gets Core's reply to a `getqrinfo` with no base
/// at that height, whatever bases it asked with, so no test checks a reply's bases against its
/// request over the rig. The chain has one branch and its tip only rises: reorgs and lowered tips
/// are built on a branching chain. It exposes no member list (D9).
pub struct CaptureRig {
    /// The engine under test.
    pub engine: MasternodeListEngine,
    /// The capture's chain.
    pub chain: RigChain,
    /// Testnet's configuration.
    pub config: LlmqConfig,
}

impl CaptureRig {
    /// An engine synced to the capture's block `tip`, with the headers to it accepted first, as a
    /// host that syncs headers before lists: Core's `qrinfo`s for the rotation cycles at or below
    /// it, then the host's tip diffs a block at a time from the newest one's work block (Core's
    /// step diffs).
    ///
    /// # Panics
    /// Panics if no captured cycle's work block is at or below `tip`, `tip` is above the
    /// capture's, or the engine refuses Core's replies.
    pub fn testnet(tip: u32) -> Self {
        let v = testnet_capture();
        let params =
            ChainParamsBuilder::new(Network::Testnet3, Release::V24_0).build().expect("params");
        let config = LlmqConfig::new(Arc::new(params)).expect("in the domain");
        let chain = RigChain::new(v, tip);
        let mut engine = MasternodeListEngine::new(Mode::Client);
        let mut work = 0;
        for q in v["qrinfos"].as_array().expect("qrinfos") {
            if u32_of(&q["request_height"]) <= tip && q["extra_share"] == false {
                let reply = P2pQrInfo::new(decode(&q["payload"]));
                work = work.max(chain.height(&reply.diff_h().block_hash()).expect("a header"));
                engine.apply_qrinfo(&reply, &chain, &config).expect("Core's qrinfo");
            }
        }
        assert!(work > 0 && work <= tip, "a cycle's work block at or below the tip");
        let mut rig = CaptureRig {
            engine,
            chain,
            config,
        };
        rig.tip_diffs(work, tip);
        rig
    }

    /// Accepts the capture's headers up to `tip` and applies the host's tip diffs to it.
    ///
    /// # Panics
    /// Panics if no tip diff base is held or Core's replies are refused.
    pub fn advance(&mut self, tip: u32) {
        let (base, _) = self
            .engine
            .tip_diff_base(&self.chain)
            .expect("the chain reads")
            .expect("a tip diff base");
        self.tip_diffs(base, tip);
    }

    /// The host's tip diffs one block at a time from `from` to `to`, each header accepted by the
    /// time its diff applies.
    fn tip_diffs(&mut self, from: u32, to: u32) {
        for h in from + 1..=to {
            if h > self.chain.tip() {
                self.chain.set_tip(h);
            }
            let diff = step_diff(h).unwrap_or_else(|| panic!("Core's step diff to {h}"));
            self.engine
                .apply_diff(&diff, &self.chain, &self.config)
                .unwrap_or_else(|e| panic!("{h}: {e}"));
        }
    }

    /// Core's reply to `request`, if a capture holds it. A `getmnlistd` gets Core's reply to
    /// exactly that request. A `getqrinfo` is matched by its height only: it gets Core's reply to
    /// a `getqrinfo` with no base and no extra share at that height, whatever bases it asked
    /// with. Any base it offers is a held list, so the engine applies that reply's diffs from
    /// genesis as well, but its bases are not the request's.
    pub fn reply(&self, request: &MnRequest) -> Option<Reply> {
        match request {
            MnRequest::MnListDiff {
                base,
                target,
                target_height,
            } => {
                let base = match base {
                    Some(hash) => Some(self.chain.height(hash)?),
                    None => None,
                };
                let diff = cores_diff(base, *target_height)?;
                (diff.block_hash() == *target).then_some(Reply::Diff(diff))
            }
            MnRequest::QrInfo {
                request_height,
                ..
            } => testnet_capture()["qrinfos"]
                .as_array()
                .expect("qrinfos")
                .iter()
                .find(|q| {
                    u32_of(&q["request_height"]) == *request_height && q["extra_share"] == false
                })
                .map(|q| Reply::QrInfo(P2pQrInfo::new(decode(&q["payload"])))),
        }
    }

    /// Answers the outstanding request `key` with Core's reply: `None` if it is not outstanding or
    /// no capture holds its reply.
    pub fn answer(&mut self, key: RequestKey) -> Option<Result<MnChange, MnReject>> {
        let request = self.engine.requests().into_iter().find(|r| r.key() == key)?;
        Some(match self.reply(&request)? {
            Reply::Diff(d) => self.engine.apply_diff(&d, &self.chain, &self.config),
            Reply::QrInfo(q) => self.engine.apply_qrinfo(&q, &self.chain, &self.config),
        })
    }

    /// Answers every outstanding request a capture holds the reply to, until none is left, and
    /// returns those it cannot answer, still outstanding.
    ///
    /// # Panics
    /// Panics if the engine refuses Core's reply.
    pub fn pump(&mut self) -> Vec<MnRequest> {
        loop {
            let (answerable, rest): (Vec<_>, Vec<_>) =
                self.engine.requests().into_iter().partition(|r| self.reply(r).is_some());
            let Some(next) = answerable.first() else {
                return rest;
            };
            self.answer(next.key()).expect("outstanding").expect("Core's reply applies");
        }
    }

    /// Reports `key` as failed by every peer.
    pub fn fail(&mut self, key: RequestKey) {
        self.engine.request_failed(key);
    }

    /// Core's ChainLock for the block at `height`, as the capture holds it.
    pub fn chainlock(&self, height: u32) -> Option<ClSig> {
        let c = testnet_capture()["chainlocks"]
            .as_array()
            .expect("chainlocks")
            .iter()
            .find(|c| u32_of(&c["height"]) == height)?;
        let mut bytes = i32::try_from(height).expect("a height").to_le_bytes().to_vec();
        bytes.extend(rpc(c["block_hash"].as_str().expect("a hash")));
        bytes.extend(hex(c["signature"].as_str().expect("a signature")));
        Some(ClSig::decode(&mut &bytes[..]).expect("a clsig"))
    }

    /// The hash and public key of Core's quorum of `llmq_type` based at `height`, as
    /// `quorum info` lists them.
    pub fn quorum(&self, llmq_type: LlmqType, height: u32) -> Option<(QuorumHash, Vec<u8>)> {
        testnet_capture()["quorums"][llmq_type.to_base().to_string()]
            .as_array()?
            .iter()
            .find(|q| u32_of(&q["height"]) == height)
            .map(|q| {
                let hash = QuorumHash::from(rpc(q["quorumHash"].as_str().expect("a hash")));
                (hash, hex(q["quorumPublicKey"].as_str().expect("a key")))
            })
    }
}

fn testnet_capture() -> &'static Value {
    static CAPTURE: OnceLock<Value> = OnceLock::new();
    CAPTURE
        .get_or_init(|| gunzip(include_bytes!("../tests/data/sb11-testnet-v24.0.captures.json.gz")))
}

fn testnet_fetch_capture() -> &'static Value {
    static CAPTURE: OnceLock<Value> = OnceLock::new();
    CAPTURE.get_or_init(|| {
        gunzip(include_bytes!("../tests/data/sb11-testnet-fetch-v24.0.captures.json.gz"))
    })
}

fn gunzip(gz: &[u8]) -> Value {
    serde_json::from_reader(flate2::read::GzDecoder::new(gz)).expect("the capture")
}

/// Core's step diff to `height`.
fn step_diff(height: u32) -> Option<P2pMnListDiff> {
    cores_diff(Some(height.checked_sub(1)?), height)
}

/// Core's `getmnlistd(base, target)` (heights; `None` a null base): the fetch capture's, or the
/// testnet capture's step diff or diff from genesis.
fn cores_diff(base: Option<u32>, target: u32) -> Option<P2pMnListDiff> {
    let is = |d: &&Value| {
        d["base_height"].as_u64().map(|b| u32::try_from(b).expect("small")) == base
            && u32_of(&d["height"]) == target
    };
    let fetched = testnet_fetch_capture()["diffs"].as_array().expect("diffs").iter();
    let v = testnet_capture();
    let captured =
        ["step", "from_genesis"].into_iter().flat_map(|k| v["diffs"][k].as_array().expect("diffs"));
    fetched.chain(captured).find(is).map(|d| P2pMnListDiff::new(decode(&d["payload"])))
}

fn decode<T: BaseCodec>(payload: &Value) -> T {
    let bytes = hex(payload.as_str().expect("a hex payload"));
    let mut r = &bytes[..];
    let t = T::decode(&mut r).expect("Core's payload");
    assert!(r.is_empty(), "one payload");
    t
}

/// An RPC hash: hex of the reversed bytes.
fn rpc(s: &str) -> [u8; 32] {
    let mut b: [u8; 32] = hex(s).try_into().expect("32 bytes");
    b.reverse();
    b
}

/// Replays one gzipped rotated capture through commitment verification, each of its lists
/// decoded by `decode`.
///
/// # Panics
/// Panics if the bytes are not a valid rotated capture. Intended only for test fixtures.
pub fn replay_rotated_capture(
    bytes: &[u8],
    decode: ListDecoder,
) -> Vec<(LlmqType, QuorumHash, QuorumState, Commitment)> {
    let mut json = Vec::new();
    flate2::read::GzDecoder::new(bytes).read_to_end(&mut json).expect("gzip");
    let v: Value = serde_json::from_slice(&json).expect("json");
    let network = match v["network"].as_str().expect("a network") {
        "mainnet" => Network::Main,
        "testnet" => Network::Testnet3,
        other => panic!("unknown capture network {other}"),
    };
    let params = ChainParamsBuilder::new(network, Release::V24_0).build().expect("params");
    let cfg = LlmqConfig::new(Arc::new(params)).expect("in the domain");
    let chain = CaptureChain::new(
        u32_of(&v["first_height"]),
        &hex(v["headers"].as_str().expect("headers")),
    );
    let engine = held(&v, &chain, &cfg, None, decode);
    v["quorums"]
        .as_array()
        .expect("quorums")
        .iter()
        .filter_map(|q| {
            let bytes = hex(q.get("commitment")?.as_str().expect("hex"));
            let qc = Commitment::decode(&mut &bytes[..]).expect("a commitment");
            let state = engine.step5(qc.clone(), u32_of(&q["mined_height"]), &chain, &cfg);
            Some((qc.llmq_type(), qc.quorum_hash(), state, qc))
        })
        .collect()
}

fn hex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex")).collect()
}

fn u32_of(v: &Value) -> u32 {
    u32::try_from(v.as_u64().expect("a number")).expect("small")
}
