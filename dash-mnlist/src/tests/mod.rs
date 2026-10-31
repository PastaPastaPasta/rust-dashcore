//! The engine against Dash Core: a v24 regtest network's `mnlistdiff`, `qrinfo`, `clsig` and
//! `isdlock` payloads (`tests/data/sb11-regtest-v24.0.captures.json.gz`, made by
//! `contrib/node/sb11-captures`), over a chain of its headers; and a second network across DIP0008
//! and V19 (`tests/data/sb11-regtest-transitions-v24.0.captures.json.gz`).

mod edges;
mod horizon;
mod lookup;
mod persist;
mod regtest;
mod retention;
mod rotated;
#[cfg(feature = "shadow")]
mod shadow;
mod step7;
mod testnet;
mod transitions;

use std::collections::HashMap;
use std::ops::RangeInclusive;
use std::sync::{Arc, OnceLock};

use dash_chain::{AncestryError, AnchoredAncestry, BlockRef, StoreError};
use dash_consensus::BlockId;
use dash_consensus::ctx::HeaderCtx;
use dash_num::{Arith256, CompactTarget};
use dash_p2p_core::{ClSig, IsdLock, MnListDiff, QrInfo};
use dash_params::LlmqType;
use dash_params::{ChainOption, ChainParamsBuilder, Network, Release};
use dash_primitives::{BlockHash, BlockHeader, MerkleRoot, QuorumHash};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;
use serde_json::Value;

use crate::{LlmqConfig, MasternodeListEngine, MnChain, MnRequest, Mode, QuorumState};

const CHAIN_LOCKS: u8 = 100;
const ROTATED: u8 = 103;
/// The first block the capture has a diff from genesis to.
const FIRST: u32 = 24;

/// A capture of `tests/data` (its `PROVENANCE.md`).
pub(crate) type Capture = &'static Value;

fn load(gz: &[u8]) -> Value {
    serde_json::from_reader(flate2::read::GzDecoder::new(gz)).expect("the capture")
}

/// The rotation and lock capture.
pub(crate) fn fixture() -> Capture {
    static FIXTURE: OnceLock<Value> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        load(include_bytes!("../../tests/data/sb11-regtest-v24.0.captures.json.gz"))
    })
}

/// The capture across DIP0008 and V19.
pub(crate) fn transitions() -> Capture {
    static FIXTURE: OnceLock<Value> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        load(include_bytes!("../../tests/data/sb11-regtest-transitions-v24.0.captures.json.gz"))
    })
}

pub(crate) fn hex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex")).collect()
}

/// An RPC hash: hex of the reversed bytes.
pub(crate) fn rpc(s: &str) -> [u8; 32] {
    let mut b: [u8; 32] = hex(s).try_into().expect("32 bytes");
    b.reverse();
    b
}

pub(crate) fn decode<T: BaseCodec>(payload: &Value) -> T {
    let bytes = hex(payload.as_str().expect("a hex payload"));
    let mut r = &bytes[..];
    let t = T::decode(&mut r).expect("Core's payload");
    assert!(r.is_empty(), "one payload");
    t
}

fn u32_of(v: &Value) -> u32 {
    u32::try_from(v.as_u64().expect("a number")).expect("small")
}

/// Regtest with the capture's chain arguments. `-vbparams` (a versionbits deployment no gate
/// reads) and `-watchquorums` (the observer's role) are not chain parameters here.
pub(crate) fn config() -> &'static LlmqConfig {
    static CONFIG: OnceLock<LlmqConfig> = OnceLock::new();
    CONFIG.get_or_init(|| config_with(&[]))
}

/// [`config`] with `extra` chain arguments after the capture's.
pub(crate) fn config_with(extra: &[&str]) -> LlmqConfig {
    config_of(fixture(), extra)
}

/// Regtest with `f`'s chain arguments, then `extra`.
pub(crate) fn config_of(f: Capture, extra: &[&str]) -> LlmqConfig {
    let args = f["provenance"]["chain_args"].as_array().expect("chain_args");
    let mut builder = ChainParamsBuilder::new(Network::Regtest, Release::V24_0);
    for arg in args.iter().map(|a| a.as_str().expect("an arg")).chain(extra.iter().copied()) {
        let (flag, value) = arg.split_once('=').expect("flag=value");
        let option = match flag {
            "-llmqtestparams" => ChainOption::LlmqTestParams,
            "-llmqtestinstantsendparams" => ChainOption::LlmqTestInstantSendParams,
            "-llmqtestplatformparams" => ChainOption::LlmqTestPlatformParams,
            "-dip3params" => ChainOption::Dip3Params,
            "-testactivationheight" => ChainOption::TestActivationHeight,
            "-vbparams" | "-watchquorums" => continue,
            other => panic!("an unmapped chain argument {other}"),
        };
        builder = builder.arg(option, value);
    }
    LlmqConfig::new(Arc::new(builder.build().expect("the capture's regtest")))
        .expect("in the domain")
}

/// The first id of [`FixtureChain::fork`]'s branch: block h on it is `BlockId(FORK + h)`.
pub(crate) const FORK: u32 = 10_000;

/// The capture's headers, height as id, from `floor` to `tip`.
pub(crate) struct FixtureChain {
    headers: Vec<HeaderCtx>,
    roots: Vec<MerkleRoot>,
    ids: HashMap<BlockHash, u32>,
    pub floor: u32,
    pub tip: u32,
    /// A height whose `header` read fails as a store would.
    pub fail_header: Option<u32>,
    /// A height whose `ancestor` read fails as a store would.
    pub fail_ancestor: Option<u32>,
    /// Headers below it are pruned while `lookup` still knows their blocks: `header` reads
    /// `BelowFloor`.
    pub header_floor: u32,
    /// A sibling branch above this height: block h on it has the capture's header and merkle
    /// root for h under [`Self::fork_hash`]`(h)`. The active chain stays the capture's unless
    /// `fork_active`.
    pub fork: Option<u32>,
    /// Whether the fork's branch is the active chain (a reorg onto it).
    pub fork_active: bool,
}

impl FixtureChain {
    /// The whole capture.
    pub fn full() -> Self {
        Self::of(fixture())
    }

    /// The whole of capture `f`.
    pub fn of(f: Capture) -> Self {
        let hashes: Vec<BlockHash> = f["block_hashes"]
            .as_array()
            .expect("block_hashes")
            .iter()
            .map(|h| BlockHash::from(rpc(h.as_str().expect("a hash"))))
            .collect();
        let raw: Vec<BlockHeader> =
            f["headers"].as_array().expect("headers").iter().map(decode).collect();
        assert_eq!(hashes.len(), raw.len());
        let mut headers = Vec::new();
        let mut times = Vec::new();
        for (height, (h, hash)) in raw.iter().zip(&hashes).enumerate() {
            if height > 0 {
                assert_eq!(h.prev_hash, hashes[height - 1], "the headers chain");
            }
            times.push(i64::from(h.time));
            let mut window: Vec<i64> = times.iter().rev().take(11).copied().collect();
            window.sort_unstable();
            headers.push(HeaderCtx {
                height: u32::try_from(height).expect("small"),
                hash: *hash,
                time: h.time,
                bits: CompactTarget::new(h.bits),
                version: h.version,
                mtp: window[window.len() / 2],
                chain_work: Arith256::ZERO,
            });
        }
        FixtureChain {
            roots: raw.iter().map(|h| h.merkle_root).collect(),
            ids: hashes
                .iter()
                .enumerate()
                .map(|(i, h)| (*h, u32::try_from(i).expect("small")))
                .collect(),
            floor: 0,
            tip: u32_of(&f["tip_height"]),
            headers,
            fail_header: None,
            fail_ancestor: None,
            header_floor: 0,
            fork: None,
            fork_active: false,
        }
    }

    /// The capture with headers only from `floor` (an anchored client's) up to `tip`.
    pub fn anchored(floor: u32, tip: u32) -> Self {
        FixtureChain {
            floor,
            tip,
            ..Self::full()
        }
    }

    pub fn id(&self, height: u32) -> BlockId {
        assert!((self.floor..=self.tip).contains(&height));
        BlockId(height)
    }

    pub fn hash(&self, height: u32) -> BlockHash {
        self.headers[height as usize].hash
    }

    /// Block `height` on the fork's branch.
    pub fn fork_hash(&self, height: u32) -> BlockHash {
        assert!(self.fork.is_some_and(|f| height > f), "above the fork");
        let mut b = *self.hash(height).as_bytes();
        b[0] ^= 0xa5;
        BlockHash::from(b)
    }

    /// `d` moved onto the fork's branch: its target and base above the fork renamed.
    pub fn on_fork(&self, mut d: MnListDiff) -> MnListDiff {
        let fork = self.fork.expect("a fork");
        for hash in [&mut d.block_hash, &mut d.base_block_hash] {
            if let Some(h) = self.ids.get(hash).copied().filter(|h| *h > fork) {
                *hash = self.fork_hash(h);
            }
        }
        d
    }

    /// An id's branch and height.
    fn branch(&self, id: BlockId) -> (bool, u32) {
        match id.0.checked_sub(FORK) {
            Some(h) => {
                assert!(self.fork.is_some_and(|f| h > f), "an id the chain never issued");
                (true, h)
            }
            None => (false, id.0),
        }
    }

    fn held(&self, height: u32) -> Result<(), AncestryError> {
        if height < self.floor {
            return Err(AncestryError::BelowFloor {
                height,
            });
        }
        assert!(height <= self.tip, "an id the chain never issued");
        Ok(())
    }
}

fn injected(at: Option<u32>, height: u32) -> Result<(), AncestryError> {
    match at {
        Some(h) if h == height => Err(AncestryError::Storage(StoreError::Io("injected".into()))),
        _ => Ok(()),
    }
}

impl AnchoredAncestry for FixtureChain {
    fn floor(&self) -> u32 {
        self.floor
    }

    fn header(&self, id: BlockId) -> Result<HeaderCtx, AncestryError> {
        let (forked, h) = self.branch(id);
        self.held(h)?;
        injected(self.fail_header, h)?;
        if h < self.header_floor {
            return Err(AncestryError::BelowFloor {
                height: h,
            });
        }
        let mut header = self.headers[h as usize];
        if forked {
            header.hash = self.fork_hash(h);
        }
        Ok(header)
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Result<Option<BlockId>, AncestryError> {
        let (forked, h) = self.branch(id);
        if height > h {
            return Ok(None);
        }
        self.held(height)?;
        injected(self.fail_ancestor, height)?;
        Ok(Some(if forked && self.fork.is_some_and(|f| height > f) {
            BlockId(FORK + height)
        } else {
            BlockId(height)
        }))
    }

    fn prev(&self, id: BlockId) -> Result<Option<BlockId>, AncestryError> {
        match self.branch(id).1.checked_sub(1) {
            None => Ok(None),
            Some(h) => self.ancestor(id, h),
        }
    }
}

impl MnChain for FixtureChain {
    fn ancestry(&self) -> &dyn AnchoredAncestry {
        self
    }

    fn lookup(&self, hash: &BlockHash) -> Result<Option<BlockId>, AncestryError> {
        let held = self.floor..=self.tip;
        if let Some(h) = self.ids.get(hash).filter(|h| held.contains(*h)) {
            return Ok(Some(BlockId(*h)));
        }
        let fork = self.fork.map_or(self.tip, |f| f.max(self.floor.saturating_sub(1)));
        Ok((fork + 1..=self.tip).find(|h| self.fork_hash(*h) == *hash).map(|h| BlockId(FORK + h)))
    }

    fn active_tip(&self) -> Result<BlockRef, AncestryError> {
        let (id, hash) = if self.fork_active {
            (BlockId(FORK + self.tip), self.fork_hash(self.tip))
        } else {
            (BlockId(self.tip), self.hash(self.tip))
        };
        Ok(BlockRef {
            id,
            hash,
            height: self.tip,
        })
    }

    fn merkle_root(&self, id: BlockId) -> Result<Option<MerkleRoot>, AncestryError> {
        Ok(self.roots.get(self.branch(id).1 as usize).copied())
    }
}

/// `f`'s `kind` diff to `height`.
fn diff_to(f: Capture, kind: &str, height: u32) -> &'static Value {
    f["diffs"][kind]
        .as_array()
        .unwrap_or_else(|| panic!("{kind} diffs"))
        .iter()
        .find(|d| u32_of(&d["height"]) == height)
        .unwrap_or_else(|| panic!("a {kind} diff to the height"))
}

/// The capture's `mnlistdiff` from each block's parent, by target height.
pub(crate) fn step_diff(height: u32) -> MnListDiff {
    step_diff_of(fixture(), height)
}

/// `f`'s `mnlistdiff` from the block before `height`.
pub(crate) fn step_diff_of(f: Capture, height: u32) -> MnListDiff {
    let d = diff_to(f, "step", height);
    assert_eq!(u32_of(&d["base_height"]), height - 1);
    decode(&d["payload"])
}

/// The capture's `mnlistdiff` from the null base to `height`.
pub(crate) fn genesis_diff(height: u32) -> MnListDiff {
    genesis_diff_of(fixture(), height)
}

/// `f`'s `mnlistdiff` from the null base to `height`.
pub(crate) fn genesis_diff_of(f: Capture, height: u32) -> MnListDiff {
    decode(&diff_to(f, "from_genesis", height)["payload"])
}

/// The capture's `qrinfo` for `request_height`, asked with `bases` (heights).
pub(crate) fn qrinfo(request_height: u32, bases: &[u32]) -> QrInfo {
    let q = fixture()["qrinfos"]
        .as_array()
        .expect("qrinfos")
        .iter()
        .find(|q| {
            u32_of(&q["request_height"]) == request_height
                && q["base_heights"]
                    .as_array()
                    .expect("bases")
                    .iter()
                    .map(u32_of)
                    .eq(bases.iter().copied())
        })
        .expect("the qrinfo");
    decode(&q["payload"])
}

pub(crate) fn messages(command: &str) -> Vec<&'static Value> {
    fixture()["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .filter(|m| m["command"] == command)
        .collect()
}

/// The captured `command` messages and whether Core verified each at capture.
fn locks<T: BaseCodec>(command: &str) -> Vec<(T, bool)> {
    messages(command)
        .into_iter()
        .map(|m| (decode(&m["payload"]), m["core_verify"] == true))
        .collect()
}

pub(crate) fn clsigs() -> Vec<(ClSig, bool)> {
    locks("clsig")
}

pub(crate) fn isdlocks() -> Vec<(IsdLock, bool)> {
    locks("isdlock")
}

pub(crate) fn llmq(t: u8) -> LlmqType {
    LlmqType::from_base(t)
}

/// The target heights of the outstanding `mnlistdiff` requests, in order.
pub(crate) fn asked_heights(engine: &MasternodeListEngine) -> Vec<u32> {
    let mut heights: Vec<u32> = engine
        .requests()
        .iter()
        .filter_map(|r| match r {
            MnRequest::MnListDiff {
                target_height,
                ..
            } => Some(*target_height),
            MnRequest::QrInfo {
                ..
            } => None,
        })
        .collect();
    heights.sort_unstable();
    heights
}

/// The status of type `t`'s held commitment for `hash`.
pub(crate) fn state(
    engine: &MasternodeListEngine,
    t: u8,
    hash: impl Into<QuorumHash>,
) -> QuorumState {
    engine.quorum(llmq(t), &hash.into()).expect("held").1
}

/// The list from genesis to `from_genesis`, then every list in `steps` one block at a time.
pub(crate) fn applied(
    chain: &dyn MnChain,
    from_genesis: u32,
    steps: RangeInclusive<u32>,
) -> MasternodeListEngine {
    let mut engine = MasternodeListEngine::new(Mode::Client);
    apply_to(&mut engine, chain, from_genesis, steps);
    engine
}

/// [`applied`] into `engine`.
pub(crate) fn apply_to(
    engine: &mut MasternodeListEngine,
    chain: &dyn MnChain,
    from_genesis: u32,
    steps: RangeInclusive<u32>,
) {
    // The captured `qrinfo`s' base: the request they answer kept it (D29).
    engine.pin_list(BlockId(QRINFO_BASE));
    engine.apply_diff(&genesis_diff(from_genesis), chain, config()).expect("Core's diff");
    for h in steps {
        engine.apply_diff(&step_diff(h), chain, config()).unwrap_or_else(|e| panic!("{h}: {e}"));
    }
}

/// The first DIP3 cycle's list from genesis, then every list to the tip one block at a time.
pub(crate) fn stepped(chain: &FixtureChain) -> MasternodeListEngine {
    applied(chain, FIRST, FIRST + 1..=chain.tip)
}

/// The base the captured cycle 288 `qrinfo` diffs from.
pub(crate) const QRINFO_BASE: u32 = 240;

/// Both captured cycles' `qrinfo`s applied: the rows the lock checks select from.
pub(crate) fn apply_cycles(engine: &mut MasternodeListEngine, chain: &dyn MnChain) {
    for (request, bases) in [(284, &[][..]), (288, &[QRINFO_BASE][..])] {
        engine.apply_qrinfo(&qrinfo(request, bases), chain, config()).expect("Core's qrinfo");
    }
}

/// Every list and both cycles' rows.
pub(crate) fn synced(chain: &FixtureChain) -> MasternodeListEngine {
    let mut engine = stepped(chain);
    apply_cycles(&mut engine, chain);
    engine
}

/// Core's quorums of `llmq_type` (`quorum info`): base height, quorum hash, public key and
/// members' operator keys.
pub(crate) struct CoreQuorum {
    pub height: u32,
    pub hash: [u8; 32],
    pub public_key: Vec<u8>,
    pub operator_keys: Vec<Vec<u8>>,
}

pub(crate) fn core_quorums(llmq_type: u8) -> Vec<CoreQuorum> {
    fixture()["quorums"][llmq_type.to_string()]
        .as_array()
        .expect("the type's quorums")
        .iter()
        .map(|q| CoreQuorum {
            height: u32_of(&q["height"]),
            hash: rpc(q["quorumHash"].as_str().expect("a hash")),
            public_key: hex(q["quorumPublicKey"].as_str().expect("a key")),
            operator_keys: q["members"]
                .as_array()
                .expect("members")
                .iter()
                .map(|m| hex(m["pubKeyOperator"].as_str().expect("a key")))
                .collect(),
        })
        .collect()
}
