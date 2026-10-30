//! CS5b's targets: `diff_check_tx_inputs` and `diff_sequence_locks`, the node's
//! `check_tx_inputs` and relative lock times against Core's `Consensus::CheckTxInputs` and
//! `CalculateSequenceLocks` / `EvaluateSequenceLocks` through the oracle (O2a).
//! `diff_check_tx_inputs` also compares the sigop counts that read the spent coins
//! (`GetP2SHSigOpCount`, `GetTransactionSigOpCount`) against `oracle_transaction_sigops` (O1b).
//!
//! Compared: for `diff_check_tx_inputs`, the verdict, result class, reject reason, debug message
//! and fee, then the sigop counts under no flags and under P2SH whenever the view holds every
//! input's coin; for `diff_sequence_locks`, the required height and time and whether the block
//! satisfies them.
//!
//! The rules are v24's (DEC-82): at any other pin there is nothing to compare. Not compared
//! either: inputs the oracle refuses as outside its domain, and transactions other than asset
//! unlocks that break a precondition Core's callers establish with `CheckTransaction` (a
//! coinbase, whose inputs Core asserts on; outputs out of the money range, on which
//! `GetValueOut` throws). An asset unlock returns before either matters.

use std::collections::BTreeMap;
use std::convert::Infallible;

use dash_consensus::coins::{CoinRef, CoinsView, ViewOr};
use dash_consensus::script::ScriptFlags;
use dash_consensus::tx::{
    LockTimeFlags, calculate_sequence_locks, check_tx_inputs, evaluate_sequence_locks,
    p2sh_sigop_count, transaction_sigop_count,
};
use dash_consensus::{Amount, Tx, decode_tx_exact, money_range};
use dash_oracle_sys::{ContextCoin, ErrorKind, Oracle, Pin, SpentOutput};
use dash_primitives::{BlockHash, OutPoint, TxOut};

use crate::check_inputs::{Position, Verdict, Window, verdict};
use crate::checks::{chain, compare_verdicts, context};
use crate::{Compared, Divergence, Outcome, check, compare_tx_exact, divergence};

/// One coin record of a `diff_check_tx_inputs` input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoinRecord {
    /// Bit 0: a coinbase output. Bit 1: absent (the view holds no coin for the input).
    pub flags: u8,
    /// The spend height minus the coin's height.
    pub depth: i16,
    pub value: i64,
    pub script: Vec<u8>,
}

impl CoinRecord {
    pub const COINBASE: u8 = 1;
    pub const ABSENT: u8 = 2;
    const HEAD: usize = 12;
}

/// A `diff_check_tx_inputs` input:
///
/// | bytes | field |
/// |---|---|
/// | 0..4 | spend height (u32 LE, low 31 bits) |
/// | 4 | coin record count (modulo 32) |
/// | then, per record | flags (u8), depth (i16 LE), value (i64 LE), script length (u8), script |
/// | rest | the transaction (exactly) |
///
/// Record `i` is the coin input `i` spends. No view holds a coin flagged absent, one with value
/// −1 (`CTxOut`'s spent marker) or an unspendable one (`AddCoin` drops `OP_RETURN` outputs), so
/// those inputs, inputs past the records and inputs repeating an earlier input's prevout add no
/// coin. A coin's height is the spend height less the depth, clamped to `0..=i32::MAX`.
pub struct TxInputsInput<'a> {
    pub spend_height: u32,
    pub coins: Vec<CoinRecord>,
    pub tx: &'a [u8],
}

impl<'a> TxInputsInput<'a> {
    pub fn parse(bytes: &'a [u8]) -> Option<Self> {
        let spend_height = u32::from_le_bytes(bytes.get(..4)?.try_into().ok()?) & 0x7fff_ffff;
        let (&count, mut rest) = bytes.get(4..)?.split_first()?;
        let mut coins = Vec::new();
        for _ in 0..count % 32 {
            let (head, tail) = rest.split_at_checked(CoinRecord::HEAD)?;
            let (script, tail) = tail.split_at_checked(usize::from(head[11]))?;
            coins.push(CoinRecord {
                flags: head[0],
                depth: i16::from_le_bytes([head[1], head[2]]),
                value: i64::from_le_bytes(head[3..11].try_into().ok()?),
                script: script.to_vec(),
            });
            rest = tail;
        }
        Some(Self {
            spend_height,
            coins,
            tx: rest,
        })
    }

    /// The bytes [`TxInputsInput::parse`] reads back (scripts longer than 255 bytes are cut).
    pub fn encode(spend_height: u32, coins: &[CoinRecord], tx: &[u8]) -> Vec<u8> {
        let mut out = spend_height.to_le_bytes().to_vec();
        out.push(coins.len() as u8);
        for coin in coins {
            let script = &coin.script[..coin.script.len().min(255)];
            out.push(coin.flags);
            out.extend(coin.depth.to_le_bytes());
            out.extend(coin.value.to_le_bytes());
            out.push(script.len() as u8);
            out.extend(script);
        }
        out.extend(tx);
        out
    }

    /// The coins view the input describes for `tx`.
    pub fn view(&self, tx: &Tx) -> Coins {
        let mut coins = Coins::default();
        for (input, record) in tx.inputs().iter().zip(&self.coins) {
            let unspendable = record.script.first() == Some(&0x6a);
            if record.flags & CoinRecord::ABSENT != 0 || record.value == -1 || unspendable {
                continue;
            }
            let height = (i64::from(self.spend_height) - i64::from(record.depth))
                .clamp(0, i64::from(i32::MAX)) as u32;
            let out = TxOut {
                value: record.value,
                script_pubkey: record.script.clone().into(),
            };
            let coinbase = record.flags & CoinRecord::COINBASE != 0;
            coins.0.entry(input.prevout).or_insert((out, height, coinbase));
        }
        coins
    }

    /// The node's verdict and fee for `tx` over `view`.
    fn judge(&self, tx: &Tx, view: &Coins) -> (Verdict, Option<Amount>) {
        match check_tx_inputs(tx, view, self.spend_height) {
            Ok(fee) => (Ok(()), Some(fee)),
            Err(ViewOr::Reject(e)) => (verdict(Err(e)), None),
            Err(ViewOr::View(never)) => match never {},
        }
    }

    /// The node's verdict and fee, when the transaction decodes and meets the preconditions.
    pub fn ours(&self) -> Option<(Verdict, Option<Amount>)> {
        let tx = decode_tx_exact(self.tx).ok()?.into_value();
        meets_preconditions(&tx).then(|| self.judge(&tx, &self.view(&tx)))
    }

    /// The node's P2SH sigop count, when [`compare_check_tx_inputs`] compares the sigops.
    pub fn p2sh_sigops(&self) -> Option<u32> {
        let tx = decode_tx_exact(self.tx).ok()?.into_value();
        let view = self.view(&tx);
        spent_outputs(&tx, &view)?;
        p2sh_sigop_count(&tx, &view).ok()
    }
}

/// Whether `tx` meets `CheckTxInputs`' preconditions: an asset unlock (which returns first), or
/// not a coinbase with every output value and their running sum in the money range.
fn meets_preconditions(tx: &Tx) -> bool {
    let mut total: Amount = 0;
    tx.is_platform_transfer()
        || !tx.is_coinbase()
            && tx.outputs().iter().all(|output| {
                total = total.saturating_add(output.value);
                money_range(output.value) && money_range(total)
            })
}

/// Coins held as outputs, as Core's in-memory cache holds them (no compression round trip).
#[derive(Default)]
pub struct Coins(BTreeMap<OutPoint, (TxOut, u32, bool)>);

impl Coins {
    fn records(&self) -> Vec<ContextCoin> {
        self.0
            .iter()
            .map(|(op, (out, height, coinbase))| ContextCoin {
                txid: *op.hash.as_bytes(),
                vout: op.index,
                height: *height,
                coinbase: *coinbase,
                amount: out.value,
                script_pubkey: out.script_pubkey.as_bytes().to_vec(),
            })
            .collect()
    }
}

impl CoinsView for Coins {
    type Error = Infallible;

    fn coin(&self, op: &OutPoint) -> Result<Option<CoinRef<'_>>, Infallible> {
        Ok(self.0.get(op).map(|(out, height, cb)| CoinRef::from_output(out, *height, *cb)))
    }

    fn best_block(&self) -> Option<BlockHash> {
        None
    }
}

/// An oracle failure: outside its domain, a decode verdict that differs from ours (which
/// decoded), or anything else.
pub(crate) fn oracle_failure(e: &dash_oracle_sys::Error) -> Compared {
    match e.kind {
        ErrorKind::Argument | ErrorKind::Context => Ok(Outcome::OutOfDomain),
        ErrorKind::Decode | ErrorKind::Trailing => Err(divergence("decode verdict")),
        _ => Err(divergence("oracle error")),
    }
}

/// Core's decode verdict on bytes the node does not decode.
fn both_reject_decode(tx: &[u8], pin: Pin) -> Compared {
    match compare_tx_exact(tx, pin) {
        Ok(Outcome::BothReject) => Ok(Outcome::BothReject),
        _ => Err(divergence("decode verdict")),
    }
}

/// `diff_check_tx_inputs`: `CheckTxInputs` of a transaction over a coins view at a spend
/// height, and the sigop counts that read the view.
pub fn compare_check_tx_inputs(bytes: &[u8], pin: Pin) -> Compared {
    if pin != Pin::V24_0 {
        return Ok(Outcome::OutOfDomain);
    }
    let Some(input) = TxInputsInput::parse(bytes) else {
        return Ok(Outcome::OutOfDomain);
    };
    let Ok(tx) = decode_tx_exact(input.tx) else {
        return both_reject_decode(input.tx, pin);
    };
    let tx = tx.into_value();
    if !meets_preconditions(&tx) {
        return Ok(Outcome::OutOfDomain);
    }
    let view = input.view(&tx);
    let oracle = Oracle::new(pin);
    let core = match oracle.check_tx_inputs(input.tx, &view.records(), input.spend_height as i32) {
        Ok(core) => core,
        Err(e) => return oracle_failure(&e),
    };
    let (ours, fee) = input.judge(&tx, &view);
    let outcome = compare_verdicts(&core.verdict, &ours)?;
    check(core.fee == fee, "fee")?;
    compare_sigops(&oracle, input.tx, &tx, &view)?;
    Ok(outcome)
}

/// The sigop counts against `oracle_transaction_sigops`, when the view holds each input's coin
/// and no two inputs share a prevout (the oracle's spent outputs are one per input).
fn compare_sigops(oracle: &Oracle, bytes: &[u8], tx: &Tx, view: &Coins) -> Result<(), Divergence> {
    let Some(spent) = spent_outputs(tx, view) else {
        return Ok(());
    };
    for flags in [ScriptFlags::NONE, ScriptFlags::P2SH] {
        let core = oracle
            .transaction_sigops(bytes, &spent, flags.bits())
            .map_err(|_| divergence("oracle error"))?;
        check(core.legacy == tx.legacy_sigop_count(), "legacy sigops")?;
        check(Ok(core.p2sh) == p2sh_sigop_count(tx, view), "p2sh sigops")?;
        check(Ok(core.total) == transaction_sigop_count(tx, view, flags), "transaction sigops")?;
    }
    Ok(())
}

/// The outputs `tx` spends, one per input, when the view holds each and no two inputs share one.
fn spent_outputs<'v>(tx: &Tx, view: &'v Coins) -> Option<Vec<SpentOutput<'v>>> {
    let spent = tx
        .inputs()
        .iter()
        .map(|input| {
            view.0.get(&input.prevout).map(|(out, _, _)| SpentOutput {
                amount: out.value,
                script_pubkey: out.script_pubkey.as_bytes(),
            })
        })
        .collect::<Option<Vec<_>>>()?;
    (spent.len() == view.0.len()).then_some(spent)
}

/// The window lengths a `diff_sequence_locks` input selects: the tip's median time past, one
/// more header, and deeper coins.
pub const LOCK_WINDOWS: [u32; 4] = [11, 12, 30, 80];

/// A `diff_sequence_locks` input:
///
/// | bytes | field |
/// |---|---|
/// | 0..12 | a [`Position`]: the block holding the transaction is at the anchor plus the delta |
/// | 12 | window length, one of [`LOCK_WINDOWS`] |
/// | 13..17 | the lock-time flags (Core's `int`, LE) |
/// | 17 | depth count `n` |
/// | 18..18 + 2n | each input's coin depth below the block (u16 LE, modulo the window length less 10) |
/// | rest | the transaction (exactly) |
///
/// Inputs past the depths spend coins of the block itself (depth 0); a depth below genesis is
/// height 0. The modulus keeps the median time past before each coin's block inside the window,
/// where the oracle can read it.
pub struct LocksInput<'a> {
    pub window: Window,
    pub flags: u32,
    pub depths: Vec<u16>,
    pub tx: &'a [u8],
}

pub const LOCKS_HEAD: usize = Position::LEN + 6;

impl<'a> LocksInput<'a> {
    pub fn parse(bytes: &'a [u8]) -> Option<Self> {
        let position = Position::parse(bytes)?;
        let head = bytes.get(Position::LEN..LOCKS_HEAD)?;
        let len = LOCK_WINDOWS[usize::from(head[0] % 4)];
        let flags = u32::from_le_bytes(head[1..5].try_into().ok()?);
        let (depths, tx) = bytes[LOCKS_HEAD..].split_at_checked(2 * usize::from(head[5]))?;
        let depths = depths
            .chunks(2)
            .map(|d| u16::from_le_bytes([d[0], d[1]]) % (len - 10) as u16)
            .collect();
        Some(Self {
            window: position.window(len, None),
            flags,
            depths,
            tx,
        })
    }

    /// The bytes [`LocksInput::parse`] reads back.
    pub fn encode(position: &[u8], window: u8, flags: u32, depths: &[u16], tx: &[u8]) -> Vec<u8> {
        let mut out = position.to_vec();
        out.push(window);
        out.extend(flags.to_le_bytes());
        out.push(depths.len() as u8);
        for depth in depths {
            out.extend(depth.to_le_bytes());
        }
        out.extend(tx);
        out
    }

    /// The height of the coin each of `inputs` inputs spends.
    pub fn prev_heights(&self, inputs: usize) -> Vec<u32> {
        let block = self.window.tip() + 1;
        (0..inputs)
            .map(|i| block.saturating_sub(u32::from(self.depths.get(i).copied().unwrap_or(0))))
            .collect()
    }

    /// The node's required height and time for `tx`, and whether the block satisfies them.
    fn locks(&self, tx: &Tx) -> (i32, i64, bool) {
        let mut heights = self.prev_heights(tx.inputs().len());
        let ctx = self.window.ctx(0);
        let flags = LockTimeFlags::from_bits(self.flags);
        let locks = calculate_sequence_locks(tx, flags, &mut heights, &ctx);
        (locks.min_height, locks.min_time, evaluate_sequence_locks(&ctx, locks))
    }

    /// [`LocksInput::locks`], when the transaction decodes.
    pub fn ours(&self) -> Option<(i32, i64, bool)> {
        Some(self.locks(decode_tx_exact(self.tx).ok()?.value()))
    }
}

/// `diff_sequence_locks`: `CalculateSequenceLocks` and `EvaluateSequenceLocks` for a
/// transaction in the block after a window's tip.
pub fn compare_sequence_locks(bytes: &[u8], pin: Pin) -> Compared {
    if pin != Pin::V24_0 {
        return Ok(Outcome::OutOfDomain);
    }
    let Some(input) = LocksInput::parse(bytes) else {
        return Ok(Outcome::OutOfDomain);
    };
    let Ok(tx) = decode_tx_exact(input.tx) else {
        return both_reject_decode(input.tx, pin);
    };
    let tx = tx.into_value();
    let heights: Vec<i32> =
        input.prev_heights(tx.inputs().len()).into_iter().map(|h| h as i32).collect();
    let core = match Oracle::new(pin).sequence_locks(
        &chain(input.window.network),
        input.tx,
        &context(&input.window, 0),
        &heights,
        input.flags as i32,
    ) {
        Ok(core) => core,
        Err(e) => return oracle_failure(&e),
    };
    let (min_height, min_time, satisfied) = input.locks(&tx);
    check(core.min_height == min_height, "min height")?;
    check(core.min_time == min_time, "min time")?;
    check(core.satisfied == satisfied, "satisfied")?;
    Ok(Outcome::BothAccept)
}
