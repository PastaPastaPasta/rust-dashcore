//! EV3-3's `diff_mnhf_step`: a block's MNHF signals (`CMNHFManager::ProcessBlock`, with
//! `IsValidMNActivation`), the stage the next block reads (`GetSignalsStage`) and the undo
//! (`UndoBlock`) against `oracle_mnhf_step` (ABI 26; the tip-cache seed, format 2, ABI 28).
//!
//! An input is a crafted regtest chain, as `dash-oracle-sys/tests/mnhf.rs` builds them: headers
//! 600 s apart from `T0` up to a tip between 12 and 40, V20 (and MN_RR) from a height around the
//! tip, the three deployments' windows (start and timeout around the block's median time past)
//! and `useEHF`, the rows Core's store holds for the V20 headers, the bits the tip's cache entry
//! holds below V20 (`seed_tip_cache`, O2a-MNHF D13), and the block's transactions: signals with
//! repeats and any bit below `VERSIONBITS_NUM_BITS`, null or other signature bytes, a payload with
//! a trailing byte, other transaction and payload versions, and another special type carrying a
//! signal's payload. Compared: the verdict and reason, the stage before, the result, and for a
//! connected block the stage after, the undo and the stage after it.
//!
//! The node's side is `dash_dml::mnhf::MnhfView` over the same headers and rows, with a fresh
//! `MnhfManager` whose tip entry, when seeded, is set as the oracle sets it (`seed_cache`). A
//! connected block's row is then written and read back by another fresh manager.

use dash_consensus::ctx::{BlockId, ChainAncestry, HeaderCtx};
use dash_consensus::{BlockValidationResult, decode_block, decode_header};
use dash_dml::mnhf::{EhfSignals, MnhfError, MnhfManager, MnhfView};
use dash_dml::store::{EvoDelta, MemEvoStore};
use dash_num::{Arith256, CompactTarget};
use dash_oracle_sys::{
    Chain, Checkpoints, CoinsView, Context, ErrorKind, MnList, MnhfContext, MnhfStep, Oracle, Pin,
    Rejection, ResultCode, SignalChange, Verdict,
};
use dash_params::{ChainOption, ChainParams, ChainParamsBuilder, NetworkKind, Release};
use dash_types::Numeric as _;

use crate::{Compared, Divergence, Outcome, check, divergence};

/// The most edits an input makes.
pub const MAX_OPS: usize = 16;

/// The first header's time (as the oracle's crafted chains).
const T0: u32 = 1_417_713_337;
/// `nBits` of every header (regtest's powLimit).
const BITS: u32 = 0x207f_ffff;
/// `TRANSACTION_MNHF_SIGNAL`, and another special type for a payload under the wrong type.
const MNHF_TYPE: u32 = 7;
const ASSET_LOCK_TYPE: u32 = 8;
/// `VERSIONBITS_NUM_BITS`: a signal bit at or above it is outside the oracle's domain
/// (`IsValidMNActivation` asserts, O2a-MNHF D5).
const NUM_BITS: u8 = 29;
/// Regtest's versionbits deployments, in `vDeployments` order (chainparams.cpp:842-867).
const DEPLOYMENTS: [(&str, i32); 3] = [("testdummy", 28), ("v24", 13), ("evo_shares", 14)];

/// A deployment's window and `useEHF` (the other fields stay regtest's).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    pub start: i64,
    pub timeout: i64,
    pub use_ehf: bool,
}

/// A transaction of the block after its coinbase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockTx {
    /// An MNHF signal for `bit` whose signature bytes follow `sig` (0: null).
    Signal {
        bit: u8,
        sig: u8,
    },
    /// An MNHF signal payload with a trailing byte (`GetTxPayload` refuses it).
    Trailing(u8),
    /// An asset lock carrying an MNHF signal's payload (another type: no signal).
    OtherType(u8),
    /// An MNHF signal at transaction `version`: below 3 there is no payload field, so no signal;
    /// above 3 it is still special.
    TxVersion {
        bit: u8,
        version: i16,
    },
    /// An MNHF signal whose payload has `version` (`extractEHFSignal` reads any; the version
    /// check is `CheckMNHFTx`'s).
    PayloadVersion {
        bit: u8,
        version: u8,
    },
}

impl BlockTx {
    /// The bit `extractEHFSignal` reads from the transaction (`evo/mnhftx.cpp:176-188`): none for
    /// a trailing byte, another type or a non-special version.
    fn signal_bit(self) -> Option<u8> {
        match self {
            BlockTx::Signal {
                bit,
                ..
            }
            | BlockTx::PayloadVersion {
                bit,
                ..
            } => Some(bit),
            BlockTx::TxVersion {
                bit,
                version,
            } => (version >= 3).then_some(bit),
            BlockTx::Trailing(_) | BlockTx::OtherType(_) => None,
        }
    }
}

/// One `oracle_mnhf_step` input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Case {
    pub tip: u32,
    pub v20_height: u32,
    /// `None` where the deployment keeps regtest's parameters.
    pub windows: [Option<Window>; 3],
    /// `(height, signals)`: from `height` on, the row of each V20 header, until the next change.
    pub rows: Vec<(u32, Vec<(u8, i32)>)>,
    /// The bits the tip's cache entry holds, at the tip's height (below V20 only).
    pub seed: Vec<u8>,
    pub txs: Vec<BlockTx>,
    pub just_check: bool,
}

/// The median time past of the crafted header at `height` (the block's when `height = tip + 1`).
pub fn mtp(height: u32) -> i64 {
    let mut times: Vec<u32> = (height.saturating_sub(10)..=height).map(time).collect();
    times.sort_unstable();
    i64::from(times[times.len() / 2])
}

fn time(height: u32) -> u32 {
    T0 + 600 * height
}

impl Case {
    /// Core's chain arguments.
    pub fn args(&self, params: &ChainParams) -> Vec<String> {
        let mut args = vec![
            "-dip3params=2:2".to_owned(),
            format!("-testactivationheight=v20@{}", self.v20_height),
            format!("-testactivationheight=mn_rr@{}", self.v20_height),
        ];
        for ((name, _), (window, d)) in
            DEPLOYMENTS.iter().zip(self.windows.iter().zip(&params.consensus.deployments))
        {
            if let Some(w) = window {
                args.push(format!(
                    "-vbparams={name}:{}:{}:{}:{}:{}:{}:{}:{}",
                    w.start,
                    w.timeout,
                    d.min_activation_height.to_u32(),
                    d.window_size,
                    d.threshold_start,
                    d.threshold_min,
                    d.falloff_coeff,
                    u8::from(w.use_ehf)
                ));
            }
        }
        args
    }

    /// The node's parameters for [`Case::args`]; `None` where they do not build.
    pub fn params(&self) -> Option<ChainParams> {
        let base = regtest();
        let mut builder = ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0);
        for arg in self.args(&base) {
            let (name, value) = arg.split_once('=').unwrap_or((&arg, ""));
            builder = builder.arg(ChainOption::from_name(name)?, value);
        }
        builder.build().ok()
    }

    /// The headers `0..=tip`, each on the previous one's hash.
    pub fn headers(&self) -> Vec<[u8; 80]> {
        let mut out = Vec::new();
        let mut prev = [0u8; 32];
        for height in 0..=self.tip {
            let h = header(time(height), prev);
            prev = header_hash(&h);
            out.push(h);
        }
        out
    }

    /// The stepped block on `tip_header`.
    pub fn block(&self, tip_header: &[u8; 80]) -> Vec<u8> {
        block_bytes(tip_header, self.txs.iter().enumerate().map(|(i, tx)| tx_bytes(*tx, i as u8)))
    }

    /// The signal changes the oracle reads: the rows, then the seeded tip entry.
    fn changes(&self) -> Vec<SignalChange> {
        let mut changes: Vec<SignalChange> = self
            .rows
            .iter()
            .map(|(height, signals)| SignalChange {
                height: *height as i32,
                signals: signals.clone(),
            })
            .collect();
        if !self.seed.is_empty() {
            let mut signals: Vec<(u8, i32)> =
                self.seed.iter().map(|&b| (b, self.tip as i32)).collect();
            signals.sort_unstable();
            changes.push(SignalChange {
                height: self.tip as i32,
                signals,
            });
        }
        changes
    }
}

#[allow(clippy::expect_used)] // Core's parameters, constants, always build
fn regtest() -> ChainParams {
    ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0).build().expect("regtest params")
}

/// A version-4 header at `time` on `prev`, with a null merkle root and nonce.
fn header(time: u32, prev: [u8; 32]) -> [u8; 80] {
    let mut h = [0u8; 80];
    h[0..4].copy_from_slice(&4i32.to_le_bytes());
    h[4..36].copy_from_slice(&prev);
    h[68..72].copy_from_slice(&time.to_le_bytes());
    h[72..76].copy_from_slice(&BITS.to_le_bytes());
    h
}

pub fn header_hash(h: &[u8; 80]) -> [u8; 32] {
    match decode_header(&mut &h[..]) {
        Ok(header) => *header.value().hash().as_bytes(),
        Err(_) => unreachable!("80 bytes decode as a header"),
    }
}

#[allow(clippy::expect_used)] // a case holds at most MAX_OPS items
fn compact(out: &mut Vec<u8>, n: usize) {
    out.push(u8::try_from(n).expect("fewer than 0xfd items"));
}

/// A coinbase-like first transaction (`extractSignals` skips it).
fn coinbase() -> Vec<u8> {
    let mut tx = 1i32.to_le_bytes().to_vec();
    compact(&mut tx, 1);
    tx.extend([0u8; 32]);
    tx.extend(u32::MAX.to_le_bytes());
    tx.extend([2, 0x01, 0x28]);
    tx.extend(u32::MAX.to_le_bytes());
    compact(&mut tx, 1);
    tx.extend(0i64.to_le_bytes());
    tx.extend([1, 0x51]);
    tx.extend(0u32.to_le_bytes());
    tx
}

/// A transaction of `version` and `tx_type` with `payload` where it has the field (version 3 or
/// more, not a normal type), no inputs or outputs, and `salt` as its lock time (equal payloads
/// stay distinct transactions).
fn tx_at(version: i16, tx_type: u32, payload: &[u8], salt: u8) -> Vec<u8> {
    let mut tx = (u32::from(version as u16) | (tx_type << 16)).to_le_bytes().to_vec();
    compact(&mut tx, 0);
    compact(&mut tx, 0);
    tx.extend(u32::from(salt).to_le_bytes());
    if version >= 3 && tx_type != 0 {
        compact(&mut tx, payload.len());
        tx.extend(payload);
    }
    tx
}

/// A version 3 transaction of `tx_type` with `payload`.
fn special(tx_type: u32, payload: &[u8], salt: u8) -> Vec<u8> {
    tx_at(3, tx_type, payload, salt)
}

/// An `MNHFTxPayload` (version 1, `bit`, a null quorum hash) with signature bytes `sig`: 0 null,
/// 1 the compressed point at infinity, else `sig` repeated.
fn signal_payload(bit: u8, sig: u8) -> Vec<u8> {
    let mut payload = vec![1u8, bit];
    payload.extend([0u8; 32]);
    let mut signature = [0u8; 96];
    match sig {
        0 => {}
        1 => signature[0] = 0xc0,
        s => signature.fill(s),
    }
    payload.extend(signature);
    payload
}

fn tx_bytes(tx: BlockTx, salt: u8) -> Vec<u8> {
    match tx {
        BlockTx::Signal {
            bit,
            sig,
        } => special(MNHF_TYPE, &signal_payload(bit, sig), salt),
        BlockTx::Trailing(bit) => {
            let mut payload = signal_payload(bit, 0);
            payload.push(0);
            special(MNHF_TYPE, &payload, salt)
        }
        BlockTx::OtherType(bit) => special(ASSET_LOCK_TYPE, &signal_payload(bit, 0), salt),
        BlockTx::TxVersion {
            bit,
            version,
        } => tx_at(version, MNHF_TYPE, &signal_payload(bit, 0), salt),
        BlockTx::PayloadVersion {
            bit,
            version,
        } => {
            let mut payload = signal_payload(bit, 0);
            payload[0] = version;
            special(MNHF_TYPE, &payload, salt)
        }
    }
}

/// A block on `tip_header`, 600 s later, with a coinbase and then `txs`.
fn block_bytes(tip_header: &[u8; 80], txs: impl Iterator<Item = Vec<u8>>) -> Vec<u8> {
    let tip_time = u32::from_le_bytes(tip_header[68..72].try_into().unwrap_or_default());
    let txs: Vec<Vec<u8>> = txs.collect();
    let mut out = header(tip_time + 600, header_hash(tip_header)).to_vec();
    compact(&mut out, txs.len() + 1);
    out.extend(coinbase());
    txs.iter().for_each(|tx| out.extend(tx));
    out
}

/// A node block index over a line of headers, with real hashes and median times past;
/// `BlockId(h)` is the header at height `h`.
pub struct Line(Vec<HeaderCtx>);

impl Line {
    pub fn new(headers: &[[u8; 80]]) -> Self {
        let times: Vec<u32> =
            headers.iter().map(|h| u32::from_le_bytes([h[68], h[69], h[70], h[71]])).collect();
        Self(
            (0u32..)
                .zip(headers)
                .map(|(height, h)| {
                    let i = height as usize;
                    let mut last = times[i.saturating_sub(10)..=i].to_vec();
                    last.sort_unstable();
                    HeaderCtx {
                        height,
                        hash: dash_primitives::BlockHash::from(header_hash(h)),
                        time: times[i],
                        bits: CompactTarget::new(u32::from_le_bytes([h[72], h[73], h[74], h[75]])),
                        version: i32::from_le_bytes([h[0], h[1], h[2], h[3]]),
                        mtp: i64::from(last[last.len() / 2]),
                        chain_work: Arith256::ZERO,
                    }
                })
                .collect(),
        )
    }
}

impl ChainAncestry for Line {
    fn header(&self, id: BlockId) -> HeaderCtx {
        self.0[id.0 as usize]
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
        (height <= id.0).then_some(BlockId(height))
    }

    fn prev(&self, id: BlockId) -> Option<BlockId> {
        id.0.checked_sub(1).map(BlockId)
    }
}

/// Core's step, or `None` where the oracle refuses the input as outside its domain.
pub fn core_step(case: &Case) -> Result<Option<MnhfStep>, Divergence> {
    let Some(params) = case.params() else {
        return Ok(None);
    };
    let headers = case.headers();
    let block = case.block(headers.last().unwrap_or(&[0; 80]));
    let context = Context::new(
        0,
        headers,
        false,
        vec![],
        0,
        Checkpoints::Disabled,
        CoinsView::Unchecked(vec![]),
        MnList::Absent,
        vec![],
    );
    let mnhf = MnhfContext {
        just_check: case.just_check,
        signals: case.changes(),
        seed_tip_cache: !case.seed.is_empty(),
    };
    let args = case.args(&params);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match Oracle::new(Pin::V24_0).mnhf_step(&Chain::new("regtest", &args), &block, &context, &mnhf)
    {
        Ok(step) => Ok(Some(step)),
        // The crafted blocks always decode: a decode refusal is the generator's or the oracle's
        // regression, not outside the domain.
        Err(e) if matches!(e.kind, ErrorKind::Context | ErrorKind::Argument) => Ok(None),
        Err(e) if e.kind == ErrorKind::Decode => Err(divergence("oracle decode")),
        Err(_) => Err(divergence("oracle error")),
    }
}

/// The node's answer, in the oracle's shape. For a connected block the row it gives is then
/// written and read back by a fresh manager, as a restarted node reads it.
fn ours(case: &Case, params: &ChainParams) -> Result<MnhfStep, Divergence> {
    let mut headers = case.headers();
    let block_bytes = case.block(headers.last().unwrap_or(&[0; 80]));
    headers.push(block_bytes[..80].try_into().map_err(|_| divergence("block header"))?);
    let line = Line::new(&headers);
    let tip = BlockId(case.tip);
    let id = BlockId(case.tip + 1);

    let mut store = MemEvoStore::new();
    let mut row = EhfSignals::new();
    for height in case.v20_height..=case.tip {
        if let Some((_, signals)) = case.rows.iter().find(|(h, _)| *h == height) {
            row = signals.iter().copied().collect();
        }
        store.apply(&delta(&line, height, row)).map_err(|_| divergence("rows"))?;
    }
    let block = decode_block(&mut block_bytes.as_slice())
        .map_err(|_| divergence("block decode"))?
        .into_value();
    let pairs = |s: EhfSignals| s.iter().collect::<Vec<_>>();
    let manager = MnhfManager::new();
    let (step, connected_row) = {
        let evo = store.view(&line);
        let view = MnhfView {
            manager: &manager,
            params,
            chain: &line,
            store: &evo,
        };
        if !case.seed.is_empty() {
            view.seed_cache(tip, case.seed.iter().map(|&bit| (bit, case.tip as i32)).collect());
        }
        let stage = |block| view.signals_stage(Some(block)).map(pairs).map_err(|_| store_fault());
        let stage_before = stage(tip)?;
        let (verdict, processed) = match view.process_block(&block, id, case.just_check) {
            Ok(processed) => (Verdict::Accept, Some(processed)),
            Err(MnhfError::Invalid(e)) => {
                check(e.result() == BlockValidationResult::Consensus, "reject result")?;
                check(e.debug_message().is_empty(), "debug message")?;
                let rejection = Rejection {
                    result: ResultCode::Block(1),
                    reason: e.reject_reason().to_owned(),
                    debug_message: String::new(),
                    reason_truncated: false,
                    debug_truncated: false,
                };
                (Verdict::Reject(rejection), None)
            }
            Err(MnhfError::Store(_)) => return Err(store_fault()),
        };
        let (connected, connected_row) = match &processed {
            Some(p) if !case.just_check => (
                Some(dash_oracle_sys::MnhfConnected {
                    stage_after: stage(id)?,
                    undo_ok: view.undo_block(&block, id).map_err(|_| store_fault())?,
                    stage_undone: stage(tip)?,
                }),
                p.row,
            ),
            _ => (None, None),
        };
        let step = MnhfStep {
            verdict,
            stage_before,
            signals: processed.map(|p| pairs(p.signals)),
            connected,
        };
        (step, connected_row)
    };
    if let (Some(row), Some(connected)) = (connected_row, &step.connected) {
        store.apply(&delta(&line, id.0, row)).map_err(|_| divergence("row write"))?;
        let evo = store.view(&line);
        let fresh = MnhfManager::new();
        let view = MnhfView {
            manager: &fresh,
            params,
            chain: &line,
            store: &evo,
        };
        let read_back = view.signals_stage(Some(id)).map_err(|_| store_fault())?;
        check(pairs(read_back) == connected.stage_after, "row read back")?;
    }
    Ok(step)
}

fn store_fault() -> Divergence {
    divergence("store fault")
}

/// The store delta writing `signals` as the MNHF row of the header at `height`.
fn delta(line: &Line, height: u32, signals: EhfSignals) -> EvoDelta {
    EvoDelta {
        block: line.header(BlockId(height)).hash,
        height,
        dml: None,
        mined_commitments: Vec::new(),
        credit_data: None,
        credit_pool: None,
        mnhf_signals: Some(signals),
        erased_mined: Vec::new(),
        evo_tip: None,
    }
}

/// One case on both sides.
pub fn compare_case(case: &Case) -> Compared {
    let Some(core) = core_step(case)? else {
        return Ok(Outcome::OutOfDomain);
    };
    let Some(params) = case.params() else {
        return Err(divergence("chain parameters"));
    };
    let ours = ours(case, &params)?;
    match (&core.verdict, &ours.verdict) {
        (Verdict::Accept, Verdict::Accept) => {}
        (Verdict::Reject(c), Verdict::Reject(o)) => {
            check(!c.reason_truncated && c.reason == o.reason, "reject reason")?;
            check(c.result == ResultCode::Block(1), "reject result")?;
            check(c.debug_message.is_empty(), "debug message")?;
        }
        _ => return Err(divergence("verdict")),
    }
    check(core.stage_before == ours.stage_before, "stage before")?;
    check(core.signals == ours.signals, "signals")?;
    match (&core.connected, &ours.connected) {
        (None, None) => {}
        (Some(c), Some(o)) => {
            check(c.stage_after == o.stage_after, "stage after")?;
            check(c.undo_ok == o.undo_ok, "undo")?;
            check(c.stage_undone == o.stage_undone, "stage undone")?;
        }
        _ => return Err(divergence("connected")),
    }
    Ok(match core.verdict {
        Verdict::Accept => Outcome::BothAccept,
        Verdict::Reject(_) => Outcome::BothReject,
    })
}

/// Start and timeout choices around the block's median time past `m`.
fn window_time(m: i64, choice: u8, timeout: bool) -> i64 {
    const NO_TIMEOUT: i64 = i64::MAX;
    match choice % 8 {
        0 if timeout => NO_TIMEOUT,
        0 => 0,
        1 => m - 1,
        2 => m,
        3 => m + 1,
        4 => m - 600,
        5 => m - 601,
        6 => m - 3000,
        _ => m + 600,
    }
}

/// An input: `[tip, v20, flags, (op, a, b)*]`, with at most [`MAX_OPS`] ops. The tip is
/// `12 + tip % 29`; V20 starts at 2 or one of `tip - 1 ..= tip + 2`; flag bit 0 is
/// `fJustCheck`. Ops (`op % 8`):
///
/// 0. a signal for bit `a % 32` with signature bytes `b % 4`;
/// 1. by `b % 3`, bit `a % 32`: a signal payload with a trailing byte, a signal at transaction
///    version 1, 2, 4 or -1 (`b / 3 % 4`), or a signal payload of version 0, 2 or 255
///    (`b / 3 % 3`);
/// 2. an asset lock carrying a signal's payload, bit `a % 32`;
/// 3. a row change at the V20 header `a` places above V20, adding bit `b % 29` mined `b / 29`
///    blocks below it;
/// 4. the tip's cache entry holds bit `a % 29` (a tip below V20 only);
/// 5. deployment `a % 3`'s start, from `b`;
/// 6. its timeout, from `b`;
/// 7. its `useEHF`, from `b & 1`.
///
/// A signal (one `extractEHFSignal` reads) at or above `VERSIONBITS_NUM_BITS` is outside the
/// domain, as Core's `ProcessBlock` asserts on it; the same bit in a transaction that carries no
/// signal is compared.
pub fn case_from_bytes(input: &[u8]) -> Option<Case> {
    let [tip, v20, flags, ops @ ..] = input else {
        return None;
    };
    if ops.len() > 3 * MAX_OPS {
        return None;
    }
    let tip = 12 + u32::from(*tip % 29);
    let v20_height = match v20 % 5 {
        0 => 2,
        n => tip - 2 + u32::from(n),
    };
    let base = regtest();
    let mut case = Case {
        tip,
        v20_height,
        windows: [None; 3],
        rows: Vec::new(),
        seed: Vec::new(),
        txs: Vec::new(),
        just_check: flags & 1 != 0,
    };
    let m = mtp(tip + 1);
    for &[op, a, b] in ops.as_chunks::<3>().0 {
        match op % 8 {
            0 => case.txs.push(BlockTx::Signal {
                bit: a % 32,
                sig: b % 4,
            }),
            1 => case.txs.push(match b % 3 {
                0 => BlockTx::Trailing(a % 32),
                1 => BlockTx::TxVersion {
                    bit: a % 32,
                    version: [1, 2, 4, -1][usize::from(b / 3 % 4)],
                },
                _ => BlockTx::PayloadVersion {
                    bit: a % 32,
                    version: [0, 2, 255][usize::from(b / 3 % 3)],
                },
            }),
            2 => case.txs.push(BlockTx::OtherType(a % 32)),
            3 if v20_height <= tip => {
                let height = v20_height + u32::from(a) % (tip - v20_height + 1);
                let signal = (b % NUM_BITS, height.saturating_sub(u32::from(b / NUM_BITS)) as i32);
                match case.rows.iter_mut().find(|(h, _)| *h == height) {
                    Some((_, signals)) => {
                        if signals.iter().all(|(bit, _)| *bit != signal.0) {
                            signals.push(signal);
                            signals.sort_unstable();
                        }
                    }
                    None => {
                        case.rows.push((height, vec![signal]));
                        case.rows.sort_unstable();
                    }
                }
            }
            4 if tip < v20_height && !case.seed.contains(&(a % NUM_BITS)) => {
                case.seed.push(a % NUM_BITS);
            }
            5..=7 => {
                let i = usize::from(a % 3);
                let d = &base.consensus.deployments[i];
                let w = case.windows[i].get_or_insert(Window {
                    start: d.start_time,
                    timeout: d.timeout,
                    use_ehf: d.use_ehf,
                });
                match op % 8 {
                    5 => w.start = window_time(m, b, false),
                    6 => w.timeout = window_time(m, b, true),
                    _ => w.use_ehf = b & 1 != 0,
                }
            }
            _ => {}
        }
    }
    if case.txs.iter().filter_map(|tx| tx.signal_bit()).any(|bit| bit >= NUM_BITS) {
        return None;
    }
    Some(case)
}

/// The registered target's comparison.
pub fn compare_mnhf_step(input: &[u8], pin: Pin) -> Compared {
    if pin != Pin::V24_0 {
        return Ok(Outcome::OutOfDomain);
    }
    match case_from_bytes(input) {
        Some(case) => compare_case(&case),
        None => Ok(Outcome::OutOfDomain),
    }
}

/// The target's start: the regtest parameters and their deployment order, checked once.
pub fn start(_: Pin) {
    let p = regtest();
    let bits: Vec<i32> = p.consensus.deployments.iter().map(|d| d.bit).collect();
    assert_eq!(bits, DEPLOYMENTS.map(|(_, bit)| bit), "regtest's deployments");
}

/// The target's public seeds: every op at a few arguments on each V20 placement, with and
/// without `fJustCheck`, and the crafted oracle cases' shapes (a held bit, a re-signal around
/// its window start, the V20 boundary's cache entry). Call bytes only, judged when run.
pub fn seed_inputs() -> Vec<Vec<u8>> {
    let mut seeds = Vec::new();
    for v20 in 0..5u8 {
        for flags in 0..2u8 {
            seeds.push(vec![27, v20, flags]);
            for op in 0..8u8 {
                for (a, b) in [(13, 0), (14, 1), (28, 2), (5, 3), (1, 29), (2, 60)] {
                    seeds.push(vec![27, v20, flags, op, a, b]);
                }
            }
            // Two signals, one held; a bit twice; a signal beside a held one.
            seeds.push(vec![27, v20, flags, 3, 0, 13, 0, 13, 0]);
            seeds.push(vec![27, v20, flags, 0, 13, 0, 0, 5, 0, 0, 13, 0]);
            seeds.push(vec![27, v20, flags, 3, 0, 13, 0, 14, 0]);
            seeds.push(vec![27, v20, flags, 3, 0, 28, 0, 28, 0]);
            // A held signal expiring at v24's moved start, then re-signalled.
            for b in 0..8u8 {
                seeds.push(vec![27, v20, flags, 3, 0, 13 + 29 * 2, 5, 1, b, 0, 13, 0]);
            }
            // The tip's cache entry below V20.
            seeds.push(vec![27, v20, flags, 4, 13, 0, 0, 14, 0]);
            seeds.push(vec![27, v20, flags, 4, 13, 0, 0, 13, 0]);
            seeds.push(vec![27, v20, flags, 4, 28, 0]);
            seeds.push(vec![27, v20, flags, 4, 5, 0, 0, 13, 0]);
            // A signal bit at `VERSIONBITS_NUM_BITS`: outside the domain. The same bit in
            // transactions that carry no signal: compared.
            seeds.push(vec![27, v20, flags, 0, 29, 0]);
            for (op, b) in [(1, 0), (1, 1), (1, 4), (1, 10), (2, 0)] {
                seeds.push(vec![27, v20, flags, op, 30, b]);
            }
            // Each transaction and payload version, and a signal beside one.
            for b in 0..12u8 {
                seeds.push(vec![27, v20, flags, 1, 13, b]);
                seeds.push(vec![27, v20, flags, 1, 13, b, 0, 13, 0]);
            }
        }
    }
    seeds
}
