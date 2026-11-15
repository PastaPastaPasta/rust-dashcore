//! CS3's L1 inputs and the node's side of their checks, shared by `node-l1` (which runs Core's
//! side through the oracle) and this crate's tests (which replay Core's recorded verdicts).
//!
//! A contextual input describes a chain position the way the oracle's context does (O2a,
//! `oracle.h` "chain contexts"): a window of consecutive headers ending at the tip, and below
//! it placeholder entries with zero time, bits and version and hashes no block has. [`Window`]
//! is that chain as the node's views see it (`ChainAncestry`, `DeploymentView`,
//! `CheckpointView`), with `BlockId(h)` the entry at height `h`.
//!
//! Input formats (every one total: a short input is [`None`], never a panic):
//! - `diff_check_transaction`: the transaction's bytes.
//! - `diff_check_block`: a selector byte (network, then `check_pow` and `check_merkle` bits),
//!   then the block's bytes.
//! - `diff_contextual_check_tx`, `diff_contextual_check_block`: a [`Position`], a window-length
//!   byte, then the transaction's or block's bytes.
//! - `diff_contextual_check_block_header`: a [`Position`] and the candidate's fields
//!   ([`HeaderInput`]).
//!
//! The boundary rows built in these formats are in `checks_rows.rs`.

#![allow(dead_code, reason = "each includer uses a subset")]

use std::sync::OnceLock;

use dash_consensus::block::{check_block, contextual_check_block};
use dash_consensus::ctx::{CheckpointView, Deployment, DeploymentView, bls_ctx_for_block};
use dash_consensus::header::{HeaderParams, MAX_FUTURE_BLOCK_TIME, contextual_check_block_header};
use dash_consensus::pow::get_next_work_required;
use dash_consensus::script::{ScriptNum, push_data};
use dash_consensus::tx::{check_transaction, contextual_check_transaction};
use dash_consensus::validation::ResultClass;
use dash_consensus::{
    Block, BlockId, ChainAncestry, ConsensusCtx, HeaderCtx, Tx, ValidationError, decode_block,
    decode_header, decode_tx_exact,
};
use dash_num::{Arith256, CompactTarget};
use dash_params::{
    BuriedDeployment, ChainOption, ChainParamsBuilder, ChainSetup, Network as CoreNetwork, Release,
};
use dash_primitives::BlockHash;
use dash_types::Numeric as _;

/// The networks the inputs select, with the chain selection Core and the node both use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Network {
    Main,
    Test,
    Regtest,
    /// `-devnet=cs3`.
    Devnet,
}

pub const DEVNET_NAME: &str = "cs3";

impl Network {
    pub const ALL: [Network; 4] = [Network::Main, Network::Test, Network::Regtest, Network::Devnet];

    pub fn from_byte(byte: u8) -> Self {
        Self::ALL[usize::from(byte % 4)]
    }

    pub fn byte(self) -> u8 {
        self as u8
    }

    /// Core's chain name and arguments (the oracle's `Chain`).
    pub fn core_chain(self) -> (&'static str, &'static [&'static str]) {
        match self {
            Network::Main => ("main", &[]),
            Network::Test => ("test", &[]),
            Network::Regtest => ("regtest", &[]),
            Network::Devnet => ("devnet", &["-devnet=cs3"]),
        }
    }

    /// v24's chain parameters for the network.
    pub fn params(self) -> &'static ChainSetup {
        static PARAMS: [OnceLock<ChainSetup>; 4] = [const { OnceLock::new() }; 4];
        PARAMS[self as usize].get_or_init(|| {
            let kind = match self {
                Network::Main => CoreNetwork::Main,
                Network::Test => CoreNetwork::Testnet3,
                Network::Regtest => CoreNetwork::Regtest,
                Network::Devnet => CoreNetwork::Devnet,
            };
            let mut builder = ChainParamsBuilder::new(kind, Release::V24_0);
            if self == Network::Devnet {
                builder = builder.arg(ChainOption::Devnet, DEVNET_NAME);
            }
            builder.build().expect("v24 chain parameters")
        })
    }

    pub fn header_params(self) -> &'static HeaderParams {
        static PARAMS: [OnceLock<HeaderParams>; 4] = [const { OnceLock::new() }; 4];
        PARAMS[self as usize]
            .get_or_init(|| HeaderParams::from_chain_params(self.params()).expect("pow params"))
    }

    /// How many headers below a block at `height` its header check can read: KGW's lookback
    /// (any window) in KGW's era, otherwise a BTC retarget interval's (the difficulty algorithms'
    /// longest, including testnet's minimum-difficulty walk; DGW reads 24, the median time past
    /// 11). A header input keeps only that many of its window's last headers (see
    /// [`Position::window_tail`]): neither side reads a header below them.
    pub fn history(self, height: u32) -> u32 {
        let c = &self.params().consensus;
        if (c.pow_kgw_height.to_u32()..c.pow_dgw_height.to_u32()).contains(&height) {
            u32::MAX
        } else {
            577
        }
    }

    /// The heights a [`Position`] is anchored at: the low heights, every buried deployment's
    /// activation, the difficulty-algorithm switches, the end of mainnet's `nBits` tolerance
    /// (quirk 9) and the checkpoints.
    pub fn anchors(self) -> &'static [u32] {
        static ANCHORS: [OnceLock<Vec<u32>>; 4] = [const { OnceLock::new() }; 4];
        ANCHORS[self as usize].get_or_init(|| {
            let p = self.params();
            let c = &p.consensus;
            let mut heights = vec![1, 2, 11, 16, 17, 68_590];
            heights.extend(
                [
                    BuriedDeployment::HeightInCoinbase,
                    BuriedDeployment::DerSig,
                    BuriedDeployment::Cltv,
                    BuriedDeployment::Csv,
                    BuriedDeployment::Dip0001,
                    BuriedDeployment::Dip0003,
                ]
                .map(|d| c.deployment_height(d).to_u32()),
            );
            heights.extend([c.pow_kgw_height.to_u32(), c.pow_dgw_height.to_u32()]);
            heights.extend(p.checkpoints.iter().map(|(h, _)| h.to_u32()));
            heights
        })
    }
}

/// splitmix64, for the synthetic windows.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// `CScript() << height`.
pub fn height_push(height: u32) -> Vec<u8> {
    match height {
        0 => vec![0x00],
        1..=16 => vec![0x50 + height as u8],
        _ => {
            let mut out = Vec::new();
            push_data(&mut out, &ScriptNum::new(i64::from(height)).encode());
            out
        }
    }
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("4 bytes"))
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().expect("8 bytes"))
}

/// An 80-byte header.
pub fn header_bytes(version: i32, prev: [u8; 32], time: u32, bits: u32) -> [u8; 80] {
    let mut h = [0u8; 80];
    h[0..4].copy_from_slice(&version.to_le_bytes());
    h[4..36].copy_from_slice(&prev);
    h[68..72].copy_from_slice(&time.to_le_bytes());
    h[72..76].copy_from_slice(&bits.to_le_bytes());
    h
}

pub fn header_hash(header: &[u8; 80]) -> BlockHash {
    decode_header(&mut &header[..]).expect("80 bytes decode").value().hash()
}

/// A chain position: `[network, anchor, delta (i16 LE), seed (u64 LE)]`. The tip is the
/// network's anchor (indexed modulo the anchor count) plus the delta, less one, so the checked
/// block is at the anchor plus the delta; the seed generates the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Position {
    pub network: Network,
    pub tip: u32,
    pub seed: u64,
}

impl Position {
    pub const LEN: usize = 12;

    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::LEN {
            return None;
        }
        let network = Network::from_byte(bytes[0]);
        let anchors = network.anchors();
        let anchor = anchors[usize::from(bytes[1]) % anchors.len()];
        let delta = i64::from(u16_at(bytes, 2) as i16);
        let tip = (i64::from(anchor) + delta - 1).clamp(0, i64::from(u32::MAX >> 1)) as u32;
        Some(Self {
            network,
            tip,
            seed: u64_at(bytes, 4),
        })
    }

    /// The bytes that [`Position::parse`] reads back as a position with this network and seed,
    /// anchored at anchor `anchor` with `delta`.
    pub fn encode(network: Network, anchor: u8, delta: i16, seed: u64) -> Vec<u8> {
        let mut out = vec![network.byte(), anchor];
        out.extend(delta.to_le_bytes());
        out.extend(seed.to_le_bytes());
        out
    }

    /// The position whose checked block is at `height` (at least 1), anchored at the nearest
    /// anchor.
    pub fn encode_height(network: Network, height: u32, seed: u64) -> Vec<u8> {
        let anchors = network.anchors();
        let (index, anchor) =
            anchors.iter().enumerate().min_by_key(|(_, a)| a.abs_diff(height)).expect("anchors");
        let delta = i16::try_from(i64::from(height) - i64::from(*anchor)).expect("near an anchor");
        Self::encode(network, index as u8, delta, seed)
    }

    /// A synthetic window of `len` headers ending at the tip (from height 0 when the tip is
    /// lower): linked, with times stepping by the network's spacing plus noise (some steps go
    /// back) and bits drawn around one of a few targets.
    pub fn window(&self, len: u32, checkpoints: Option<u32>) -> Window {
        self.window_tail(len, u32::MAX, checkpoints)
    }

    /// The last `kept` (at least one) headers of [`Position::window`]`(len, checkpoints)`,
    /// byte for byte: the whole window is generated (and linked) and the headers below are
    /// dropped, so a check that reads no further back sees the same chain.
    pub fn window_tail(&self, len: u32, kept: u32, checkpoints: Option<u32>) -> Window {
        let params = self.network.params();
        let mut rng = Rng(self.seed);
        let len = len.clamp(1, self.tip + 1);
        let first = self.tip + 1 - len;
        let spacing = params.consensus.pow_target_spacing.clamp(1, 1 << 20) as u64;
        let base_bits = [0x1e0f_fff0u32, 0x1b0f_ffff, 0x1c00_ffff, 0x207f_ffff, 0x1d00_ffff]
            [rng.below(5) as usize];
        let mut time = 1_400_000_000u32.wrapping_add(rng.below(400_000_000) as u32);
        let mut prev = [0u8; 32];
        let mut headers = Vec::with_capacity(len as usize);
        let mut hashes = Vec::with_capacity(len as usize);
        for _ in 0..len {
            let bits = match rng.below(8) {
                0 => rng.next() as u32,
                1 | 2 => base_bits ^ (rng.below(0x1_0000) as u32),
                _ => base_bits,
            };
            let header = header_bytes(4, prev, time, bits);
            let hash = header_hash(&header);
            prev = *hash.as_bytes();
            headers.push(header);
            hashes.push(hash);
            let step = rng.below(4 * spacing) as i64 - (spacing / 2) as i64;
            time = time.wrapping_add_signed(step as i32);
        }
        let dropped = len - kept.clamp(1, len);
        headers.drain(..dropped as usize);
        hashes.drain(..dropped as usize);
        Window::new(self.network, first + dropped, headers, hashes, checkpoints)
    }
}

/// The chain the contextual checks run on: `headers` at `first_height..=tip`, placeholders
/// below, and with checkpoints on, the best header's height (the oracle's
/// `Checkpoints::Enabled { best_header_height }`).
pub struct Window {
    pub network: Network,
    pub first_height: u32,
    pub headers: Vec<[u8; 80]>,
    entries: Vec<HeaderCtx>,
    pub best_header_height: Option<u32>,
}

impl Window {
    /// `headers` with their `hashes` (X11, the bulk of building a long window, so computed once).
    fn new(
        network: Network,
        first_height: u32,
        headers: Vec<[u8; 80]>,
        hashes: Vec<BlockHash>,
        best_header_height: Option<u32>,
    ) -> Self {
        assert!(!headers.is_empty(), "a window has a tip");
        let mut window = Self {
            network,
            first_height,
            entries: Vec::with_capacity(headers.len()),
            headers: Vec::new(),
            best_header_height,
        };
        for ((header, hash), height) in headers.iter().zip(hashes).zip(first_height..) {
            window.entries.push(HeaderCtx {
                height,
                hash,
                time: u32_at(header, 68),
                bits: CompactTarget::new(u32_at(header, 72)),
                version: u32_at(header, 0) as i32,
                mtp: 0,
                chain_work: Arith256::ZERO,
            });
            let mtp = window.median_time_past(height);
            window.entries.last_mut().expect("pushed").mtp = mtp;
        }
        window.headers = headers;
        window
    }

    /// `headers` at `first_height..`, each hashed here.
    pub fn from_headers(
        network: Network,
        first_height: u32,
        headers: Vec<[u8; 80]>,
        best_header_height: Option<u32>,
    ) -> Self {
        let hashes = headers.iter().map(header_hash).collect();
        Self::new(network, first_height, headers, hashes, best_header_height)
    }

    pub fn tip(&self) -> u32 {
        self.first_height + self.entries.len() as u32 - 1
    }

    pub fn tip_header(&self) -> &[u8; 80] {
        self.headers.last().expect("a window has a tip")
    }

    pub fn tip_time(&self) -> u32 {
        self.entry(self.tip()).time
    }

    pub fn params(&self) -> &'static ChainSetup {
        self.network.params()
    }

    fn entry(&self, height: u32) -> HeaderCtx {
        match height.checked_sub(self.first_height) {
            Some(i) => self.entries[i as usize],
            // The oracle's placeholders: zero fields under a hash no block has.
            None => {
                let mut hash = [0xa5; 32];
                hash[..4].copy_from_slice(&height.to_le_bytes());
                HeaderCtx {
                    height,
                    hash: BlockHash::from(hash),
                    time: 0,
                    bits: CompactTarget::new(0),
                    version: 0,
                    mtp: 0,
                    chain_work: Arith256::ZERO,
                }
            }
        }
    }

    /// `GetMedianTimePast()`: the median of the times of `height` and its ten predecessors
    /// (fewer near genesis).
    // Core: chain.h:283 CBlockIndex::GetMedianTimePast
    pub fn median_time_past(&self, height: u32) -> i64 {
        let mut times: Vec<i64> =
            (height.saturating_sub(10)..=height).map(|h| i64::from(self.entry(h).time)).collect();
        times.sort_unstable();
        times[times.len() / 2]
    }

    /// The context of a block on top of the tip, with the node's clock at `now`.
    pub fn ctx(&self, now: i64) -> ConsensusCtx<'_> {
        let params = self.params();
        let height = self.tip() + 1;
        ConsensusCtx {
            params,
            height,
            prev: Some(BlockId(self.tip())),
            chain: self,
            deployments: self,
            checkpoints: self,
            bls: bls_ctx_for_block(params, height),
            median_time_past: self.median_time_past(self.tip()),
            header_check_time: now,
        }
    }

    /// Buried deployments by height; the oracle seeds no versionbits deployment (DEFINED).
    fn active_at_height(&self, height: i64, deployment: Deployment) -> bool {
        match deployment {
            Deployment::Buried(d) => {
                height >= i64::from(self.params().consensus.deployment_height(d).to_u32())
            }
            Deployment::Versionbits(_) => false,
        }
    }

    /// The node's `GetNextWorkRequired` for a block at `time` on top of the tip. Only for
    /// building inputs: a window too short for the algorithm reads placeholders, which the
    /// oracle refuses (and the comparison then skips).
    pub fn expected_bits(&self, time: u32) -> u32 {
        let pow = self.network.header_params().pow();
        get_next_work_required(self, BlockId(self.tip()), time, pow).to_base()
    }
}

impl ChainAncestry for Window {
    fn header(&self, id: BlockId) -> HeaderCtx {
        assert!(id.0 <= self.tip(), "an id above the tip");
        self.entry(id.0)
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
        (height <= id.0).then_some(BlockId(height))
    }

    fn prev(&self, id: BlockId) -> Option<BlockId> {
        id.0.checked_sub(1).map(BlockId)
    }
}

impl DeploymentView for Window {
    fn active_after(&self, prev: Option<BlockId>, deployment: Deployment) -> bool {
        let height = prev.map_or(0, |p| i64::from(p.0) + 1);
        self.active_at_height(height, deployment)
    }

    fn active_at(&self, block: BlockId, deployment: Deployment) -> bool {
        self.active_at_height(i64::from(block.0), deployment)
    }
}

impl CheckpointView for Window {
    /// The highest checkpoint above the tip and at most the best header: the ones the index
    /// holds besides the window's own headers, which cannot be above the tip.
    fn highest_checkpoint_in_index(&self) -> Option<u32> {
        let (tip, best) = (self.tip(), self.best_header_height?);
        self.params()
            .checkpoints
            .iter()
            .map(|(h, _)| h.to_u32())
            .filter(|&h| h > tip && h <= best)
            .max()
    }
}

/// The node's verdict in Core's terms: `Err((result, reason, debug message))`.
pub type Verdict = Result<(), (i32, String, String)>;

pub fn verdict<C: ResultClass>(result: Result<(), ValidationError<C>>) -> Verdict {
    result.map_err(|e| {
        (e.result().as_i32(), e.reject_reason().to_owned(), e.debug_message().to_owned())
    })
}

/// The parts of a `diff_check_block` input.
pub struct CheckBlockInput<'a> {
    pub network: Network,
    pub check_pow: bool,
    pub check_merkle: bool,
    pub block: &'a [u8],
}

impl<'a> CheckBlockInput<'a> {
    pub fn parse(bytes: &'a [u8]) -> Option<Self> {
        let (&selector, block) = bytes.split_first()?;
        Some(Self {
            network: Network::from_byte(selector),
            check_pow: selector & 4 != 0,
            check_merkle: selector & 8 != 0,
            block,
        })
    }
}

fn decode_block_exact(bytes: &[u8]) -> Option<Block> {
    let mut cursor = bytes;
    let block = decode_block(&mut cursor).ok()?;
    cursor.is_empty().then(|| block.into_value())
}

/// The parts of a `diff_contextual_check_tx` or `diff_contextual_check_block` input.
pub struct ContextualInput<'a> {
    pub window: Window,
    pub object: &'a [u8],
}

/// The window lengths a contextual tx or block input selects: one header, or enough for the
/// median time past.
const OBJECT_WINDOWS: [u32; 4] = [11, 11, 12, 1];

impl<'a> ContextualInput<'a> {
    pub fn parse(bytes: &'a [u8]) -> Option<Self> {
        let position = Position::parse(bytes)?;
        let (&len, object) = bytes.get(Position::LEN..)?.split_first()?;
        Some(Self {
            window: position.window(OBJECT_WINDOWS[usize::from(len % 4)], None),
            object,
        })
    }
}

/// A `diff_contextual_check_block_header` input after its [`Position`]:
///
/// | bytes | field |
/// |---|---|
/// | 12 | window length, one of [`HEADER_WINDOWS`] |
/// | 13 | version: one of [`HEADER_VERSIONS`], or the next four bytes |
/// | 14..18 | `nTime` minus the tip's (i32) |
/// | 18 | bits: the node's expected value, that value with the next four bytes XORed in, or those bytes |
/// | 19..23 | raw bits, XOR mask, or version |
/// | 23..25 | the clock minus (`nTime` − `MAX_FUTURE_BLOCK_TIME`) (i16) |
/// | 25 | checkpoints: off when even |
/// | 26..30 | the best header's height above the tip (u32, modulo 2^22) |
pub struct HeaderInput {
    pub window: Window,
    pub header: [u8; 80],
    /// The node's clock (the oracle's adjusted time).
    pub now: i64,
}

pub const HEADER_INPUT_LEN: usize = 30;

/// The window lengths a header input selects: the median time past, DGW's 24 headers and a
/// margin, a BTC retarget interval on mainnet, and KGW's `PastBlocksMax` lookback (outside
/// KGW's era, only the last [`Network::history`] headers are kept).
pub const HEADER_WINDOWS: [u32; 8] = [11, 24, 25, 30, 40, 100, 577, 4040];

pub const HEADER_VERSIONS: [i32; 7] = [1, 2, 3, 4, 0x2000_0000, 0, -1];

impl HeaderInput {
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        Self::with_window(bytes, |position| position.network.history(position.tip + 1))
    }

    /// The input `bytes` give with its whole window rather than the last
    /// [`Network::history`] headers: the same chain to every check that reads no further back.
    pub fn whole(bytes: &[u8]) -> Option<Self> {
        Self::with_window(bytes, |_| u32::MAX)
    }

    fn with_window(bytes: &[u8], kept: impl FnOnce(&Position) -> u32) -> Option<Self> {
        if bytes.len() < HEADER_INPUT_LEN {
            return None;
        }
        let position = Position::parse(bytes)?;
        let best = (bytes[25] % 2 == 1).then(|| position.tip + (u32_at(bytes, 26) & 0x3f_ffff));
        let len = HEADER_WINDOWS[usize::from(bytes[12] % 8)];
        let window = position.window_tail(len, kept(&position), best);
        let (header, now) = Self::candidate(&window, bytes);
        Some(Self {
            window,
            header,
            now,
        })
    }

    /// The checked header and the clock `bytes` give on top of `window`.
    fn candidate(window: &Window, bytes: &[u8]) -> ([u8; 80], i64) {
        let raw = u32_at(bytes, 19);
        let version =
            HEADER_VERSIONS.get(usize::from(bytes[13] % 8)).copied().unwrap_or(raw as i32);
        let time = window.tip_time().wrapping_add_signed(u32_at(bytes, 14) as i32);
        let bits = match bytes[18] % 4 {
            1 => window.expected_bits(time) ^ raw,
            2 => raw,
            _ => window.expected_bits(time),
        };
        let header = header_bytes(version, *window.entry(window.tip()).hash.as_bytes(), time, bits);
        let now = i64::from(time) - MAX_FUTURE_BLOCK_TIME + i64::from(u16_at(bytes, 23) as i16);
        (header, now)
    }
}

/// Header inputs with the longest window (the only one longer than [`Network::history`]) at
/// every network's anchor, the candidate 300 s before the tip with the expected bits: the cases
/// on which [`HeaderInput::parse`] and [`HeaderInput::whole`] must agree.
pub fn header_window_inputs() -> Vec<Vec<u8>> {
    let longest = HEADER_WINDOWS.len() as u8 - 1;
    let mut inputs = Vec::new();
    for network in Network::ALL {
        for anchor in 0..network.anchors().len() as u8 {
            let mut bytes = Position::encode(network, anchor, 1, u64::from(anchor));
            bytes.extend([longest, 3]); // version 4
            bytes.extend((-300i32).to_le_bytes());
            bytes.resize(HEADER_INPUT_LEN, 0);
            inputs.push(bytes);
        }
    }
    inputs
}

/// A parsed input of one of the CS3 targets.
pub enum Input<'a> {
    CheckTransaction(&'a [u8]),
    CheckBlock(CheckBlockInput<'a>),
    ContextualTx(ContextualInput<'a>),
    ContextualBlock(ContextualInput<'a>),
    ContextualHeader(HeaderInput),
}

impl<'a> Input<'a> {
    /// `bytes` as an input of the CS3 target named `target`; `None` when too short for its
    /// format.
    pub fn parse(target: &str, bytes: &'a [u8]) -> Option<Self> {
        Some(match target {
            "diff_check_transaction" => Input::CheckTransaction(bytes),
            "diff_check_block" => Input::CheckBlock(CheckBlockInput::parse(bytes)?),
            "diff_contextual_check_tx" => Input::ContextualTx(ContextualInput::parse(bytes)?),
            "diff_contextual_check_block" => Input::ContextualBlock(ContextualInput::parse(bytes)?),
            "diff_contextual_check_block_header" => {
                Input::ContextualHeader(HeaderInput::parse(bytes)?)
            }
            _ => panic!("not a CS3 target: {target}"),
        })
    }

    /// The node's verdict; `None` when the object does not decode (exactly).
    pub fn ours(&self) -> Option<Verdict> {
        Some(match self {
            Input::CheckTransaction(bytes) => {
                verdict(check_transaction(&decode_tx_exact(bytes).ok()?.into_value()))
            }
            Input::CheckBlock(i) => verdict(check_block(
                &decode_block_exact(i.block)?,
                i.network.header_params(),
                i.check_pow,
                i.check_merkle,
            )),
            Input::ContextualTx(i) => {
                let tx: Tx = decode_tx_exact(i.object).ok()?.into_value();
                verdict(contextual_check_transaction(&tx, &i.window.ctx(0)))
            }
            Input::ContextualBlock(i) => {
                verdict(contextual_check_block(&decode_block_exact(i.object)?, &i.window.ctx(0)))
            }
            Input::ContextualHeader(i) => {
                let header =
                    decode_header(&mut &i.header[..]).expect("80 bytes decode").into_value();
                let params = i.window.network.header_params();
                verdict(contextual_check_block_header(&header, &i.window.ctx(i.now), params))
            }
        })
    }
}

/// The node's verdict on `input` for the CS3 target named `target` (`None` when the input does
/// not parse or its object does not decode).
pub fn ours(target: &str, input: &[u8]) -> Option<Verdict> {
    Input::parse(target, input)?.ours()
}
