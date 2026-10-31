//! O2a-CJ1's target `diff_coinjoin`: dash-coinjoin's session-free rules against Core's
//! src/coinjoin through the oracle (ABI 26, `oracle.h` "CoinJoin rules").
//!
//! Byte 0 selects a case (modulo 8); the rest is the case's input, read with the primitives
//! below. A short input is [`Outcome::OutOfDomain`].
//!
//! | case | input | compared |
//! |---|---|---|
//! | 0 rule | op (u8 % 11), then its arity of ints | the result |
//! | 1 entry | kind (bit 0: demotion), denom, inputs, outputs | verdict, `PoolMessage` |
//! | 2 in/outs | network, flags (bit 0: rebalance shapes, bit 1: final tx), denom, view, inputs, outputs | verdict, `PoolMessage`, collateral consumed, the session-denomination counts |
//! | 3 collateral | network, flags (bit 0: the mempool accepts, bit 1: lock time 1), view, inputs, outputs | validity, test-accept calls |
//! | 4 hashes | kind (bit 0: dstx), message | decode verdict, signature hash, the dsq's hash |
//! | 5 signature | flags (bit 0: dstx, bit 1: legacy key), key, message | decode verdict, signature verdict |
//! | 6 queue time | now, dsq | decode verdict, out of bounds |
//! | 7 dstx structure | network, V24 state (u8 % 5), tip height (u16 LE), dstx | decode verdict, validity, possibly valid post-V24 |
//!
//! Primitives:
//! - int (`t`): an even `t` picks [`PICKS`]`[t / 2 % len]`; an odd one is followed by an i64 LE.
//! - value (`t`): `t % 4` = 0: a standard denomination (`t >> 2`); 1: `t >> 2` collaterals;
//!   2: an i64 LE follows; 3: a standard denomination (`t >> 3`) plus one (bit 2) or minus one.
//! - script (`s`): below 0xc0 the P2PKH script with every hash byte `s`; above, by `s % 4`,
//!   `OP_RETURN`, empty, P2SH, and a P2PKH-length script ending in `OP_CHECKSIGVERIFY`.
//! - output: a value, then a script. Inputs and outputs: a count (u8 % 21), then the items.
//! - outpoint (`b`): 0xff the null outpoint; with bit 7 output `b & 15` of mempool transaction
//!   `(b >> 4) & 3`; otherwise output `b & 15` of chain transaction `(b >> 4) & 7` ([`chain_txid`]).
//! - view: coins (u8 % 25, each a chain outpoint (bit 7 ignored), a value and a script), mempool
//!   transactions (u8 % 5, each 1..=3 chain outpoints and up to 4 outputs), and a lock mask (bits
//!   0..4: mempool transaction i; bits 4..8: chain transaction i - 4).
//! - denom (`d`): below 0x80 `[0, 1, 2, 4, 8, 16, 32, 3][d % 8]`; otherwise an i32 LE follows.
//! - key (`k`): below 0xf0 operator key `k % 4` ([`operator_key`]) as its case's scheme
//!   serializes it; otherwise 48 raw bytes.
//! - now (`m`): with bit 0, the dsq's time plus `(m as i8) >> 1`; otherwise an i64 LE.
//!
//! What a view cannot hold is dropped, so both sides see the same view: a coin repeating an
//! outpoint, one with value −1 or an unspendable script (Core's coins tip holds neither), and a
//! mempool transaction repeating a txid or spending an outpoint another one spends (Core's
//! mempool holds no such pair). Not compared (outside the domain): rule arguments the oracle
//! refuses (`ORACLE_E_ARG`: ints outside a rule's range), sessions at denomination 0 and
//! collaterals on which Core's sums overflow (the oracle refuses exactly those, the harness
//! checks that it does, and the node's answer is still computed; a refused collateral must be
//! invalid to the node),
//! `ValidateFinalTxComposition` larger counts above `u64::MAX / 10` (Core's `size_t` products wrap
//! where dash-coinjoin saturates; no transaction has that many inputs), operator keys that are
//! not canonical in their scheme (a masternode list holds only canonical keys). Any other oracle
//! error diverges, and so does [`BlsVerdict::Unsupported`]. Both sides read the harness's
//! mempool answer, so the node's own `MempoolAccept` (ATMP) is not tested here. O2a-CJ1 notes
//! D1–D21 give the reasons.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use dash_coinjoin::collateral::{MAX_RAW_TX_FEE, MempoolAccept, is_collateral_valid};
use dash_coinjoin::denom::{
    self, COLLATERAL_AMOUNT, MAX_COLLATERAL_AMOUNT, MAX_MONEY, STANDARD_DENOMINATIONS,
};
use dash_coinjoin::dstx::{
    PoolLimits, check_dstx_signature, dstx_signature_hash, is_valid_structure, txid,
};
use dash_coinjoin::inouts::{
    CoinView, InOutsKind, ViewCoin, is_pay_to_public_key_hash, is_valid_in_outs,
    validate_demotion_entry, validate_final_tx_composition, validate_promotion_entry,
};
use dash_coinjoin::pool::PoolMessage;
use dash_coinjoin::queue::{check_queue_signature, is_time_out_of_bounds};
use dash_consensus::money_range;
use dash_oracle_sys::{
    Checkpoints, CjEntryKind, CjMessage, CjRule, CjView, CoinsView, Context, ContextCoin,
    DEPLOYMENT_V24, ErrorKind, MnList, Oracle, Pin, ThresholdState,
};
use dash_p2p_core::{DsQueue, DsTx};
use dash_params::{Bip9Deployment, DeploymentPos};
use dash_pkc::bls::core::{BlsPkValue, BlsVerdict};
use dash_pkc::bls::{BlsScIetf, BlsSchemeKind, BlsSecretKey};
use dash_primitives::{OutPoint, Transaction, TxHash, TxIn, TxOut, TxType};
use dash_types::codec::BaseCodec;
use dash_types::{Hashable, Numeric};

use crate::check_inputs::Network;
use crate::checks::chain;
use crate::{Compared, Divergence, Outcome, check, divergence};

/// The case selectors (byte 0).
pub const CASE_RULE: u8 = 0;
pub const CASE_ENTRY: u8 = 1;
pub const CASE_IN_OUTS: u8 = 2;
pub const CASE_COLLATERAL: u8 = 3;
pub const CASE_HASHES: u8 = 4;
pub const CASE_SIGNATURE: u8 = 5;
pub const CASE_QUEUE_TIME: u8 = 6;
pub const CASE_DSTX_STRUCTURE: u8 = 7;

/// The values an even int tag picks: the rules' boundaries.
pub const PICKS: [i64; 32] = [
    0,
    1,
    -1,
    2,
    3,
    4,
    5,
    8,
    9,
    10,
    11,
    16,
    20,
    32,
    i32::MAX as i64,
    i32::MIN as i64,
    (i32::MAX / 2) as i64,
    (i32::MAX / 2) as i64 + 1,
    i64::MAX,
    i64::MIN,
    MAX_MONEY,
    COLLATERAL_AMOUNT,
    MAX_COLLATERAL_AMOUNT,
    MAX_COLLATERAL_AMOUNT + 1,
    STANDARD_DENOMINATIONS[0],
    STANDARD_DENOMINATIONS[1],
    STANDARD_DENOMINATIONS[2],
    STANDARD_DENOMINATIONS[3],
    STANDARD_DENOMINATIONS[4],
    STANDARD_DENOMINATIONS[4] - 1,
    STANDARD_DENOMINATIONS[0] + 1,
    2 * COLLATERAL_AMOUNT,
];

/// The rules of case 0, by op, with their argument counts.
const RULES: [(CjRule, usize); 11] = [
    (CjRule::AmountToDenomination, 1),
    (CjRule::DenominationToAmount, 1),
    (CjRule::IsCollateralAmount, 1),
    (CjRule::AmountPriority, 1),
    (CjRule::LargerAdjacentDenom, 1),
    (CjRule::SmallerAdjacentDenom, 1),
    (CjRule::AdjacentDenominations, 2),
    (CjRule::GapThreshold, 1),
    (CjRule::ShouldPromote, 4),
    (CjRule::ShouldDemote, 4),
    (CjRule::FinalTxComposition, 4),
];

const DENOMS: [i32; 8] = [0, 1, 2, 4, 8, 16, 32, 3];
/// The largest `ValidateFinalTxComposition` larger-denomination count compared (the counts Core
/// multiplies by `PROMOTION_RATIO`): beyond it Core's `size_t` product wraps where
/// dash-coinjoin's saturates (D10).
const MAX_COMPOSITION_COUNT: i64 = (u64::MAX / denom::PROMOTION_RATIO as u64) as i64;
const MAX_ITEMS: u8 = 21;
const MAX_COINS: u8 = 25;
const MAX_MEMPOOL: u8 = 5;

/// The txid of chain transaction `i` (0..8): every byte `0xc0 + i`.
pub fn chain_txid(i: u8) -> TxHash {
    TxHash::from_lendian([0xc0 + (i & 7); 32])
}

/// Core's test `P2PKHScript(tag)`.
pub fn p2pkh(tag: u8) -> Vec<u8> {
    let mut s = vec![0x76, 0xa9, 0x14];
    s.extend([tag; 20]);
    s.extend([0x88, 0xac]);
    s
}

/// Operator key `i` (0..4): the basic-scheme secret key from 32 bytes `0xa0 + i`.
pub fn operator_key(i: u8) -> &'static BlsSecretKey<BlsScIetf> {
    static KEYS: [OnceLock<BlsSecretKey<BlsScIetf>>; 4] = [const { OnceLock::new() }; 4];
    let i = i % 4;
    KEYS[usize::from(i)].get_or_init(|| {
        #[allow(clippy::expect_used)]
        BlsSecretKey::from_ikm(&[0xa0 + i; 32]).expect("a fixed key")
    })
}

/// Operator key `i`'s public key as `scheme` serializes it.
pub fn operator_pubkey(i: u8, scheme: BlsSchemeKind) -> [u8; 48] {
    let canonical = operator_key(i).public_key().to_bytes();
    #[allow(clippy::expect_used)]
    BlsPkValue::from_canonical(&canonical, BlsSchemeKind::Basic)
        .expect("a valid key")
        .to_bytes(scheme)
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn u8(&mut self) -> Option<u8> {
        let (&b, rest) = self.0.split_first()?;
        self.0 = rest;
        Some(b)
    }

    fn array<const N: usize>(&mut self) -> Option<[u8; N]> {
        let (head, rest) = self.0.split_first_chunk::<N>()?;
        self.0 = rest;
        Some(*head)
    }

    fn i64(&mut self) -> Option<i64> {
        self.array().map(i64::from_le_bytes)
    }

    fn rest(self) -> &'a [u8] {
        self.0
    }

    /// An int.
    fn int(&mut self) -> Option<i64> {
        let t = self.u8()?;
        if t & 1 == 0 {
            Some(PICKS[usize::from(t >> 1) % PICKS.len()])
        } else {
            self.i64()
        }
    }

    fn value(&mut self) -> Option<i64> {
        let t = self.u8()?;
        Some(match t % 4 {
            0 => STANDARD_DENOMINATIONS[usize::from(t >> 2) % 5],
            1 => COLLATERAL_AMOUNT * i64::from(t >> 2),
            2 => self.i64()?,
            _ => {
                STANDARD_DENOMINATIONS[usize::from(t >> 3) % 5]
                    + if t & 4 != 0 {
                        1
                    } else {
                        -1
                    }
            }
        })
    }

    fn script(&mut self) -> Option<Vec<u8>> {
        let s = self.u8()?;
        if s < 0xc0 {
            return Some(p2pkh(s));
        }
        Some(match s % 4 {
            0 => vec![0x6a],
            1 => vec![],
            2 => {
                let mut p2sh = vec![0xa9, 0x14];
                p2sh.extend([s; 20]);
                p2sh.push(0x87);
                p2sh
            }
            _ => {
                let mut near = p2pkh(s);
                near[24] = 0xad;
                near
            }
        })
    }

    fn output(&mut self) -> Option<TxOut> {
        let value = self.value()?;
        Some(TxOut {
            value,
            script_pubkey: self.script()?.into(),
        })
    }

    fn outputs(&mut self, max: u8) -> Option<Vec<TxOut>> {
        let n = self.u8()? % max;
        (0..n).map(|_| self.output()).collect()
    }

    /// An outpoint; `mempool` are the view's mempool txids.
    fn outpoint(&mut self, mempool: &[TxHash]) -> Option<OutPoint> {
        let b = self.u8()?;
        Some(if b == 0xff {
            OutPoint {
                hash: TxHash::from_lendian([0; 32]),
                index: u32::MAX,
            }
        } else if b & 0x80 != 0 {
            OutPoint {
                // A mempool transaction the view lacks: a txid nothing holds.
                hash: mempool
                    .get(usize::from((b >> 4) & 3))
                    .copied()
                    .unwrap_or_else(|| TxHash::from_lendian([0xee; 32])),
                index: u32::from(b & 15),
            }
        } else {
            chain_outpoint(b)
        })
    }

    fn inputs(&mut self, mempool: &[TxHash]) -> Option<Vec<TxIn>> {
        let n = self.u8()? % MAX_ITEMS;
        (0..n).map(|_| self.outpoint(mempool).map(txin)).collect()
    }

    fn denom(&mut self) -> Option<i32> {
        let d = self.u8()?;
        if d < 0x80 {
            Some(DENOMS[usize::from(d % 8)])
        } else {
            self.array().map(i32::from_le_bytes)
        }
    }

    fn network(&mut self) -> Option<Network> {
        self.u8().map(Network::from_byte)
    }

    fn view(&mut self) -> Option<View> {
        let mut view = View::default();
        for _ in 0..self.u8()? % MAX_COINS {
            let outpoint = chain_outpoint(self.u8()? & 0x7f);
            let out = self.output()?;
            if out.value != -1
                && !dash_consensus::coins::is_unspendable(out.script_pubkey.as_bytes())
            {
                view.coins.entry(outpoint).or_insert(out);
            }
        }
        let mut spent = BTreeSet::new();
        for _ in 0..self.u8()? % MAX_MEMPOOL {
            let n = self.u8()? % 3 + 1;
            let inputs: Vec<TxIn> = (0..n)
                .map(|_| self.u8().map(|b| txin(chain_outpoint(b & 0x7f))))
                .collect::<Option<_>>()?;
            let tx = transaction(inputs, self.outputs(5)?, 0);
            let tx_hash = txid(&tx);
            let mut prevouts = BTreeSet::new();
            let conflicts =
                tx.inputs.iter().any(|i| spent.contains(&i.prevout) || !prevouts.insert(i.prevout));
            if conflicts || view.mempool.iter().any(|m| txid(m) == tx_hash) {
                continue;
            }
            spent.extend(prevouts);
            view.mempool.push(tx);
        }
        let mask = self.u8()?;
        for (i, tx) in view.mempool.iter().enumerate() {
            if mask & (1 << i) != 0 {
                view.locked.insert(txid(tx));
            }
        }
        for i in 0..4u8 {
            if mask & (0x10 << i) != 0 {
                view.locked.insert(chain_txid(i));
            }
        }
        Some(view)
    }
}

fn chain_outpoint(b: u8) -> OutPoint {
    OutPoint {
        hash: chain_txid(b >> 4),
        index: u32::from(b & 15),
    }
}

fn txin(prevout: OutPoint) -> TxIn {
    TxIn {
        prevout,
        script_sig: Vec::new().into(),
        sequence: u32::MAX,
    }
}

fn transaction(inputs: Vec<TxIn>, outputs: Vec<TxOut>, lock_time: u32) -> Transaction {
    Transaction {
        version: 2,
        tx_type: TxType::Spend,
        inputs,
        outputs,
        lock_time,
        extra_payload: Vec::new(),
    }
}

fn wire<T: BaseCodec>(value: &T) -> Vec<u8> {
    let mut out = Vec::new();
    value.encode(&mut out);
    out
}

/// The coins, mempool and locks of a case, as dash-coinjoin reads them (and the mempool's
/// answer for the collateral case).
#[derive(Default)]
struct View {
    coins: BTreeMap<OutPoint, TxOut>,
    mempool: Vec<Transaction>,
    locked: BTreeSet<TxHash>,
    answer: Option<i64>,
    test_accept_calls: Cell<u32>,
}

impl View {
    fn mempool_txids(&self) -> Vec<TxHash> {
        self.mempool.iter().map(txid).collect()
    }

    fn cj_view(&self) -> CjView {
        CjView {
            coins: self
                .coins
                .iter()
                .map(|(outpoint, out)| ContextCoin {
                    txid: outpoint.hash.to_lendian(),
                    vout: outpoint.index,
                    height: 1,
                    coinbase: false,
                    amount: out.value,
                    script_pubkey: out.script_pubkey.as_bytes().to_vec(),
                })
                .collect(),
            mempool: self.mempool.iter().map(wire).collect(),
            locked: self.locked.iter().map(|txid| txid.to_lendian()).collect(),
        }
    }
}

impl CoinView for View {
    /// `CCoinsViewMemPool::GetCoin`: a mempool transaction's output (none past its outputs),
    /// else the tip's coin; value −1 is a spent coin.
    fn coin(&self, outpoint: &OutPoint) -> Option<ViewCoin> {
        let (out, in_mempool) = match self.mempool.iter().find(|tx| txid(tx) == outpoint.hash) {
            Some(tx) => (tx.outputs.get(usize::try_from(outpoint.index).ok()?)?.clone(), true),
            None => (self.coins.get(outpoint)?.clone(), false),
        };
        (out.value != -1).then_some(ViewCoin {
            out,
            in_mempool,
        })
    }

    fn is_instantsend_locked(&self, txid: &TxHash) -> bool {
        self.locked.contains(txid)
    }
}

impl MempoolAccept for View {
    fn test_accept(&self, _tx: &Transaction) -> Option<i64> {
        self.test_accept_calls.set(self.test_accept_calls.get() + 1);
        self.answer
    }
}

/// Core's answer, `None` when the failure is one `expected` accepts; any other failure diverges.
fn answer<T>(
    result: dash_oracle_sys::Result<T>,
    expected: impl FnOnce(ErrorKind) -> bool,
) -> Result<Option<T>, Divergence> {
    match result {
        Ok(v) => Ok(Some(v)),
        Err(e) if expected(e.kind) => Ok(None),
        Err(_) => Err(divergence("oracle error")),
    }
}

/// Core's answer, `None` for an input outside the oracle's domain (`ORACLE_E_ARG`).
fn domain<T>(result: dash_oracle_sys::Result<T>) -> Result<Option<T>, Divergence> {
    answer(result, |kind| kind == ErrorKind::Argument)
}

/// Core's answer to a call the harness built within the oracle's domain: any failure diverges.
fn strict<T>(result: dash_oracle_sys::Result<T>) -> Result<T, Divergence> {
    result.map_err(|_| divergence("oracle error"))
}

/// Core's decode of a message: `None` when its deserializer rejects the bytes or leaves some.
fn core_message<T>(result: dash_oracle_sys::Result<T>) -> Result<Option<T>, Divergence> {
    answer(result, |kind| matches!(kind, ErrorKind::Decode | ErrorKind::Trailing))
}

/// The message decoded as a whole, as the oracle reads it.
fn decode_whole<T: BaseCodec>(bytes: &[u8]) -> Option<T> {
    let mut cursor = bytes;
    T::decode(&mut cursor).ok().filter(|_| cursor.is_empty())
}

/// Both sides' decode verdicts agree; `Some` with both values when both decode.
fn both_decode<C, O>(core: Option<C>, ours: Option<O>) -> Result<Option<(C, O)>, Divergence> {
    match (core, ours) {
        (Some(c), Some(o)) => Ok(Some((c, o))),
        (None, None) => Ok(None),
        _ => Err(divergence("decode verdict")),
    }
}

/// `diff_coinjoin`.
pub fn compare_coinjoin(bytes: &[u8], pin: Pin) -> Compared {
    if pin != Pin::V24_0 {
        return Ok(Outcome::OutOfDomain);
    }
    let Some((&case, rest)) = bytes.split_first() else {
        return Ok(Outcome::OutOfDomain);
    };
    let mut r = Reader(rest);
    let oracle = Oracle::new(pin);
    let compared = match case % 8 {
        CASE_RULE => rule(&oracle, &mut r),
        CASE_ENTRY => entry(&oracle, &mut r),
        CASE_IN_OUTS => in_outs(&oracle, &mut r),
        CASE_COLLATERAL => collateral(&oracle, &mut r),
        CASE_HASHES => hashes(&oracle, r),
        CASE_SIGNATURE => signature(&oracle, r),
        CASE_QUEUE_TIME => queue_time(&oracle, r),
        _ => dstx_structure(&oracle, r),
    };
    compared.unwrap_or(Ok(Outcome::OutOfDomain))
}

/// The outcome of a comparison in which both sides gave the verdict `accepted`.
fn outcome(accepted: bool) -> Outcome {
    if accepted {
        Outcome::BothAccept
    } else {
        Outcome::BothReject
    }
}

fn rule(oracle: &Oracle, r: &mut Reader<'_>) -> Option<Compared> {
    let (rule, arity) = RULES[usize::from(r.u8()?) % RULES.len()];
    let args: Vec<i64> = (0..arity).map(|_| r.int()).collect::<Option<_>>()?;
    if rule == CjRule::FinalTxComposition && args[2..].iter().any(|&a| a > MAX_COMPOSITION_COUNT) {
        return None;
    }
    Some(compare_rule(oracle, rule, &args))
}

fn compare_rule(oracle: &Oracle, rule: CjRule, args: &[i64]) -> Compared {
    let Some(core) = domain(oracle.cj_rule(rule, args))? else {
        return Ok(Outcome::OutOfDomain);
    };
    // Core took the arguments, so each is in its rule's range: an int one fits i32, a size is
    // non-negative (and a multiplied one at most MAX_COMPOSITION_COUNT, from the caller).
    let int = |i: usize| i32::try_from(args[i]).map_err(|_| divergence("argument domain"));
    let size = |i: usize| usize::try_from(args[i]).map_err(|_| divergence("argument domain"));
    let ours: i64 = match rule {
        CjRule::AmountToDenomination => denom::amount_to_denomination(args[0]).into(),
        CjRule::DenominationToAmount => denom::denomination_to_amount(int(0)?),
        CjRule::IsCollateralAmount => denom::is_collateral_amount(args[0]).into(),
        CjRule::AmountPriority => denom::calculate_amount_priority(args[0]).into(),
        CjRule::LargerAdjacentDenom => denom::larger_adjacent_denom(int(0)?).into(),
        CjRule::SmallerAdjacentDenom => denom::smaller_adjacent_denom(int(0)?).into(),
        CjRule::AdjacentDenominations => denom::are_adjacent_denominations(int(0)?, int(1)?).into(),
        CjRule::GapThreshold => denom::gap_threshold(int(0)?).into(),
        CjRule::ShouldPromote => {
            denom::should_promote_denoms(int(0)?, int(1)?, int(2)?, int(3)?).into()
        }
        CjRule::ShouldDemote => {
            denom::should_demote_denoms(int(0)?, int(1)?, int(2)?, int(3)?).into()
        }
        CjRule::FinalTxComposition => {
            validate_final_tx_composition(size(0)?, size(1)?, size(2)?, size(3)?).into()
        }
    };
    check(ours == core, "rule result")?;
    Ok(Outcome::BothAccept)
}

fn entry(oracle: &Oracle, r: &mut Reader<'_>) -> Option<Compared> {
    let demotion = r.u8()? & 1 != 0;
    let session_denom = r.denom()?;
    let inputs = r.inputs(&[])?;
    let outputs = r.outputs(MAX_ITEMS)?;
    Some(compare_entry(oracle, demotion, session_denom, &inputs, &outputs))
}

fn compare_entry(
    oracle: &Oracle,
    demotion: bool,
    session_denom: i32,
    inputs: &[TxIn],
    outputs: &[TxOut],
) -> Compared {
    let kind = if demotion {
        CjEntryKind::Demotion
    } else {
        CjEntryKind::Promotion
    };
    let core = strict(oracle.cj_entry(
        kind,
        &wire(&inputs.to_vec()),
        &wire(&outputs.to_vec()),
        session_denom,
    ))?;
    let ours = if demotion {
        validate_demotion_entry(inputs, outputs, session_denom)
    } else {
        validate_promotion_entry(inputs, outputs, session_denom)
    };
    check(ours.is_ok() == core.accepted, "entry verdict")?;
    let message = ours.err().unwrap_or(PoolMessage::MsgNoErr);
    check(message as i32 == core.message, "entry message")?;
    Ok(outcome(core.accepted))
}

fn in_outs(oracle: &Oracle, r: &mut Reader<'_>) -> Option<Compared> {
    let network = r.network()?;
    let flags = r.u8()?;
    let session_denom = r.denom()?;
    let view = r.view()?;
    let inputs = r.inputs(&view.mempool_txids())?;
    let outputs = r.outputs(MAX_ITEMS)?;
    Some(compare_in_outs(
        oracle,
        network,
        &view,
        session_denom,
        flags & 1 != 0,
        flags & 2 != 0,
        &inputs,
        &outputs,
    ))
}

#[allow(clippy::too_many_arguments)]
fn compare_in_outs(
    oracle: &Oracle,
    network: Network,
    view: &View,
    session_denom: i32,
    allow_rebalance_shapes: bool,
    final_tx: bool,
    inputs: &[TxIn],
    outputs: &[TxOut],
) -> Compared {
    let core = oracle.cj_in_outs(
        &chain(network),
        &view.cj_view(),
        &wire(&inputs.to_vec()),
        &wire(&outputs.to_vec()),
        session_denom,
        allow_rebalance_shapes,
        final_tx,
    );
    let kind = if final_tx {
        InOutsKind::FinalTx
    } else {
        InOutsKind::Entry
    };
    let ours = is_valid_in_outs(view, inputs, outputs, session_denom, allow_rebalance_shapes, kind);
    // No session has denomination 0, where Core's fee sums add unchecked amounts: the oracle
    // refuses exactly those calls (D6). The node's answer is still computed, so it must not panic.
    match core {
        Err(e) if e.kind == ErrorKind::Argument => {
            check(session_denom == 0, "ORACLE_E_ARG at a session denomination")?;
            return Ok(Outcome::OutOfDomain);
        }
        _ => check(session_denom != 0, "denomination 0 not refused")?,
    }
    let core = strict(core)?;
    check(ours.is_ok() == core.accepted, "in/outs verdict")?;
    let (message, consume, counts) = match ours {
        Ok(counts) => (PoolMessage::MsgNoErr, false, [counts.inputs as u64, counts.outputs as u64]),
        Err(rejection) => (rejection.message, rejection.consume_collateral, [0, 0]),
    };
    check(message as i32 == core.message, "in/outs message")?;
    check(consume == core.consume_collateral, "consume collateral")?;
    check(counts == core.counts, "session denomination counts")?;
    Ok(outcome(core.accepted))
}

fn collateral(oracle: &Oracle, r: &mut Reader<'_>) -> Option<Compared> {
    let network = r.network()?;
    let flags = r.u8()?;
    let view = r.view()?;
    let inputs = r.inputs(&view.mempool_txids())?;
    let outputs = r.outputs(MAX_ITEMS)?;
    let tx = transaction(inputs, outputs, u32::from(flags & 2 != 0));
    Some(compare_collateral(oracle, network, view, &tx, flags & 1 != 0))
}

/// The values of the inputs' coins the view resolves.
fn input_values<'a>(view: &'a View, tx: &'a Transaction) -> impl Iterator<Item = i64> + 'a {
    tx.inputs.iter().filter_map(|i| view.coin(&i.prevout)).map(|c| c.out.value)
}

/// The amounts `ProcessTransaction()` would hold to `MoneyRange` before accepting `tx`:
/// `CheckTransaction`'s outputs and their running sum, and `CheckTxInputs`' coins and theirs,
/// over the inputs the view resolves. `false` when one is out of range.
fn amounts_in_range(view: &View, tx: &Transaction) -> bool {
    sums_in_range(tx.outputs.iter().map(|o| o.value)) && sums_in_range(input_values(view, tx))
}

/// Each value and each running sum in `MoneyRange`.
fn sums_in_range(values: impl IntoIterator<Item = i64>) -> bool {
    let mut sum = 0i64;
    values.into_iter().all(|v| {
        money_range(v) && {
            sum += v;
            money_range(sum)
        }
    })
}

/// Whether Core's `IsCollateralValid` sums overflow on `tx` over `view`, which the shim refuses
/// (`oracle.h`, D6): the output prefix it reaches (through the first output whose script fails),
/// then, when every script passes, the input prefix it reaches (up to the first unusable coin),
/// then, when every coin is found, the difference, which must also not be `i64::MIN`.
fn core_sums_overflow(view: &View, tx: &Transaction) -> bool {
    if tx.outputs.is_empty() || tx.lock_time != 0 {
        return false;
    }
    let mut out_sum = 0i64;
    for output in &tx.outputs {
        let Some(sum) = out_sum.checked_add(output.value) else {
            return true;
        };
        out_sum = sum;
        let script = output.script_pubkey.as_bytes();
        if !is_pay_to_public_key_hash(script) && !dash_consensus::coins::is_unspendable(script) {
            return false;
        }
    }
    let mut in_sum = 0i64;
    for input in &tx.inputs {
        let usable = view
            .coin(&input.prevout)
            .filter(|c| !c.in_mempool || view.is_instantsend_locked(&input.prevout.hash));
        let Some(coin) = usable else {
            return false;
        };
        let Some(sum) = in_sum.checked_add(coin.out.value) else {
            return true;
        };
        in_sum = sum;
    }
    in_sum.checked_sub(out_sum).is_none_or(|fee| fee == i64::MIN)
}

fn compare_collateral(
    oracle: &Oracle,
    network: Network,
    mut view: View,
    tx: &Transaction,
    accepts: bool,
) -> Compared {
    // The mempool's answer as Core's would be: it refuses amounts outside MoneyRange, and
    // otherwise (here, as the case says) accepts with the transaction's fee.
    let in_range = amounts_in_range(&view, tx);
    view.answer = (accepts && in_range).then(|| {
        input_values(&view, tx).sum::<i64>() - tx.outputs.iter().map(|o| o.value).sum::<i64>()
    });
    // The shim's refusal is compared too: exactly the inputs on which Core's sums overflow.
    let overflows = core_sums_overflow(&view, tx);
    let core = match oracle.cj_collateral(&chain(network), &view.cj_view(), &wire(tx), view.answer)
    {
        Err(e) if e.kind == ErrorKind::Argument => {
            check(overflows, "ORACLE_E_ARG without an overflow")?;
            // Overflow needs an amount outside MoneyRange, and dash-coinjoin checks every prefix
            // Core sums (D6): the node must refuse without asking the mempool.
            let ours = is_collateral_valid(&view, tx);
            check(!ours && view.test_accept_calls.get() == 0, "overflow collateral")?;
            return Ok(Outcome::OutOfDomain);
        }
        result => strict(result)?,
    };
    check(!overflows, "overflow not refused")?;
    let ours = is_collateral_valid(&view, tx);
    check(ours == core.valid, "collateral verdict")?;
    check(core.submit_calls == 0, "collateral submissions")?;
    // Out of MoneyRange dash-coinjoin stops before asking the mempool (its own range checks,
    // CJ0 D12), Core asks it and gets the refusal (D7).
    if in_range {
        check(view.test_accept_calls.get() == core.test_accept_calls, "test-accept calls")?;
    } else {
        check(view.test_accept_calls.get() == 0, "asked the mempool out of range")?;
    }
    Ok(outcome(core.valid))
}

fn message_kind(dstx: bool) -> CjMessage {
    if dstx {
        CjMessage::Dstx
    } else {
        CjMessage::Dsq
    }
}

fn hashes(oracle: &Oracle, mut r: Reader<'_>) -> Option<Compared> {
    let dstx = r.u8()? & 1 != 0;
    Some(compare_hashes(oracle, dstx, r.rest()))
}

fn compare_hashes(oracle: &Oracle, dstx: bool, msg: &[u8]) -> Compared {
    let core = core_message(oracle.cj_message_hash(message_kind(dstx), msg))?;
    let ours = if dstx {
        decode_whole::<DsTx>(msg).map(|m| (dstx_signature_hash(&m), None))
    } else {
        decode_whole::<DsQueue>(msg).map(|m| (m.signature_hash(), Some(m.hash())))
    };
    let Some((core, (sig_hash, hash))) = both_decode(core, ours)? else {
        return Ok(Outcome::BothReject);
    };
    check(sig_hash.to_lendian() == core.sig_hash, "signature hash")?;
    check(hash.map(|h| h.to_lendian()) == core.hash, "dsq hash")?;
    Ok(Outcome::BothAccept)
}

fn signature(oracle: &Oracle, mut r: Reader<'_>) -> Option<Compared> {
    let flags = r.u8()?;
    let (dstx, legacy) = (flags & 1 != 0, flags & 2 != 0);
    let k = r.u8()?;
    let pubkey = if k < 0xf0 {
        operator_pubkey(k, BlsSchemeKind::from_legacy(legacy))
    } else {
        r.array()?
    };
    Some(compare_signature(oracle, dstx, legacy, &pubkey, r.rest()))
}

/// A decoded message of the signature case.
enum Signed {
    Dstx(DsTx),
    Dsq(DsQueue),
}

fn compare_signature(
    oracle: &Oracle,
    dstx: bool,
    legacy: bool,
    pubkey: &[u8; 48],
    msg: &[u8],
) -> Compared {
    let core = core_message(oracle.cj_check_signature(message_kind(dstx), msg, pubkey, legacy))?;
    let ours = if dstx {
        decode_whole::<DsTx>(msg).map(Signed::Dstx)
    } else {
        decode_whole::<DsQueue>(msg).map(Signed::Dsq)
    };
    let Some((core, msg)) = both_decode(core, ours)? else {
        return Ok(Outcome::BothReject);
    };
    let Some(key) = BlsPkValue::from_canonical(pubkey, BlsSchemeKind::from_legacy(legacy)) else {
        return Ok(Outcome::OutOfDomain);
    };
    let verdict = match &msg {
        Signed::Dstx(dstx) => check_dstx_signature(dstx, &key),
        Signed::Dsq(dsq) => check_queue_signature(dsq, &key),
    };
    let ours = match verdict {
        BlsVerdict::Valid => true,
        BlsVerdict::Invalid => false,
        // Basic-scheme signatures never answer Unsupported (CJ0 D9): if a base-sdk bump made
        // them, every signature row would otherwise silently leave the domain.
        BlsVerdict::Unsupported => {
            return Err(divergence("Unsupported under the basic scheme (CJ0 D9)"));
        }
    };
    check(ours == core, "signature verdict")?;
    Ok(outcome(core))
}

fn queue_time(oracle: &Oracle, mut r: Reader<'_>) -> Option<Compared> {
    let m = r.u8()?;
    let raw = if m & 1 == 0 {
        Some(r.i64()?)
    } else {
        None
    };
    let msg = r.rest();
    let ours = decode_whole::<DsQueue>(msg);
    let now = match (raw, &ours) {
        (Some(now), _) => now,
        (None, Some(dsq)) => dsq.time.saturating_add(i64::from((m as i8) >> 1)),
        (None, None) => 0,
    };
    Some(compare_queue_time(oracle, msg, ours, now))
}

fn compare_queue_time(oracle: &Oracle, msg: &[u8], ours: Option<DsQueue>, now: i64) -> Compared {
    let core = core_message(oracle.cj_queue_time_out_of_bounds(msg, now))?;
    let Some((core, dsq)) = both_decode(core, ours)? else {
        return Ok(Outcome::BothReject);
    };
    check(is_time_out_of_bounds(dsq.time, now) == core, "queue time out of bounds")?;
    Ok(Outcome::BothAccept)
}

fn dstx_structure(oracle: &Oracle, mut r: Reader<'_>) -> Option<Compared> {
    let network = r.network()?;
    let state = ThresholdState::try_from(r.u8()? % 5).ok()?;
    let height = u32::from(u16::from_le_bytes(r.array()?));
    Some(compare_dstx_structure(oracle, network, state, height, r.rest()))
}

/// Whether V24 is active at a tip whose period has `state`: Core's `DeploymentActiveAt(tip)`,
/// which reads the seeded period when the tip is not the last block of one.
fn v24_active(network: Network, state: ThresholdState) -> bool {
    let deployment = network.params().deployment(DeploymentPos::V24);
    match deployment.map(|d| d.start_time) {
        Some(Bip9Deployment::ALWAYS_ACTIVE) => true,
        Some(Bip9Deployment::NEVER_ACTIVE) | None => false,
        Some(_) => state == ThresholdState::Active,
    }
}

/// The context of the structure check: one header at `height` (moved down one when it would
/// end a V24 period, where the tip and the block after it read different periods), V24 in
/// `state` for the block after it.
fn structure_context(network: Network, state: ThresholdState, height: u32) -> Context {
    let consensus = &network.params().consensus;
    let period = consensus
        .deployments
        .get(DeploymentPos::V24)
        .map(|d| d.window_size)
        .filter(|&w| w > 0)
        .unwrap_or(i64::from(consensus.miner_confirmation_window));
    let height = if (i64::from(height) + 1) % period == 0 {
        height - 1
    } else {
        height
    };
    let mut header = [0u8; 80];
    header[..4].copy_from_slice(&4i32.to_le_bytes());
    Context::new(
        height as i32,
        vec![header],
        false,
        vec![(DEPLOYMENT_V24, state)],
        0,
        Checkpoints::Disabled,
        CoinsView::Unchecked(vec![]),
        MnList::Absent,
        vec![],
    )
}

fn compare_dstx_structure(
    oracle: &Oracle,
    network: Network,
    state: ThresholdState,
    height: u32,
    msg: &[u8],
) -> Compared {
    let ctx = structure_context(network, state, height);
    let core = core_message(oracle.cj_dstx_structure(&chain(network), msg, &ctx))?;
    let Some((core, dstx)) = both_decode(core, decode_whole::<DsTx>(msg))? else {
        return Ok(Outcome::BothReject);
    };
    let ours =
        is_valid_structure(&dstx, PoolLimits::of(network.params()), v24_active(network, state));
    check(ours.valid == core.valid, "structure verdict")?;
    check(ours.possibly_valid_post_v24 == core.possibly_valid_post_v24, "possibly valid post-V24")?;
    Ok(outcome(core.valid))
}

/// Writes inputs in the format [`compare_coinjoin`] reads, for seeds and tests.
#[derive(Clone, Debug, Default)]
pub struct Writer(pub Vec<u8>);

impl Writer {
    pub fn case(case: u8) -> Self {
        Self(vec![case])
    }

    pub fn u8(mut self, b: u8) -> Self {
        self.0.push(b);
        self
    }

    pub fn bytes(mut self, bytes: &[u8]) -> Self {
        self.0.extend(bytes);
        self
    }

    /// An int holding `v` as an i64.
    pub fn int(self, v: i64) -> Self {
        self.u8(1).bytes(&v.to_le_bytes())
    }

    /// An i64 LE (the raw form of a queue-time case's now).
    pub fn int_raw(self, v: i64) -> Self {
        self.bytes(&v.to_le_bytes())
    }
}

/// A seed's mempool transaction: its chain outpoint bytes and its outputs (value tag, script
/// byte).
type SeedMempoolTx<'a> = (&'a [u8], &'a [(u8, u8)]);

/// The view of `coins` (chain outpoint byte, value tag, script byte) and `mempool` (each its
/// chain outpoint bytes and its outputs as (value tag, script byte)), with lock mask `mask`.
fn write_view(
    mut w: Writer,
    coins: &[(u8, u8, u8)],
    mempool: &[SeedMempoolTx<'_>],
    mask: u8,
) -> Writer {
    w = w.u8(coins.len() as u8);
    for &(outpoint, value, script) in coins {
        w = w.u8(outpoint).u8(value).u8(script);
    }
    w = w.u8(mempool.len() as u8);
    for (inputs, outputs) in mempool {
        w = write_outputs(w.u8(inputs.len() as u8 - 1).bytes(inputs), outputs);
    }
    w.u8(mask)
}

/// A list of outputs as (value tag, script byte).
fn write_outputs(mut w: Writer, outputs: &[(u8, u8)]) -> Writer {
    w = w.u8(outputs.len() as u8);
    for &(value, script) in outputs {
        w = w.u8(value).u8(script);
    }
    w
}

/// `w` followed by a list of outpoint bytes and a list of outputs, the end of an entry,
/// session or collateral input.
fn finish(w: Writer, inputs: &[u8], outputs: &[(u8, u8)]) -> Vec<u8> {
    write_outputs(w.u8(inputs.len() as u8).bytes(inputs), outputs).0
}

/// `w` followed by a view of chain coins `coins` (outpoint byte, raw value; coin `i`'s script is
/// the P2PKH tag `0x10 + i`) with no mempool and no locks, then `inputs` (outpoint bytes) and
/// `outputs` (raw value, script byte): the end of a session or collateral input with raw values.
pub fn finish_raw(
    mut w: Writer,
    coins: &[(u8, i64)],
    inputs: &[u8],
    outputs: &[(i64, u8)],
) -> Vec<u8> {
    w = w.u8(coins.len() as u8);
    for (i, &(outpoint, value)) in coins.iter().enumerate() {
        w = w.u8(outpoint).u8(2).int_raw(value).u8(0x10 + i as u8);
    }
    w = w.u8(0).u8(0).u8(inputs.len() as u8).bytes(inputs).u8(outputs.len() as u8);
    for &(value, script) in outputs {
        w = w.u8(2).int_raw(value).u8(script);
    }
    w.0
}

/// The value tag of standard denomination `i` (0: the largest) and of `k` collaterals.
const fn denomination(i: u8) -> u8 {
    i << 2
}
const fn collaterals(k: u8) -> u8 {
    1 | k << 2
}

/// The denom byte of denomination bit `1 << i`.
const fn denom_bit(i: u8) -> u8 {
    i + 1
}

/// The seeds' queue and broadcast time (October 2025) and masternode.
const SEED_TIME: i64 = 1_760_000_000;
const SEED_PRO_TX: [u8; 32] = [0x11; 32];

/// A queue at the smallest denomination signed by operator key 0 (`ready` as given).
pub fn seed_dsq(ready: bool) -> DsQueue {
    let mut dsq = DsQueue {
        denom: 1 << 4,
        pro_tx_hash: TxHash::from_lendian(SEED_PRO_TX),
        time: SEED_TIME,
        ready,
        sig: Vec::new(),
    };
    dsq.sig = sign(&dsq.signature_hash());
    dsq
}

/// A broadcast of a mix with `inputs` inputs and `outputs` outputs at the smallest
/// denomination, signed by operator key 0.
pub fn seed_dstx(inputs: u16, outputs: u16) -> DsTx {
    // Inputs below 128 spend chain_outpoint(i); the rest go on in each chain transaction's
    // outputs from 16. Every input is distinct.
    let outpoint = |i: u16| OutPoint {
        hash: chain_txid((i >> 4) as u8),
        index: u32::from(i & 15) + 16 * u32::from(i >> 7),
    };
    let tx = transaction(
        (0..inputs).map(outpoint).map(txin).collect(),
        (0..outputs)
            .map(|i| TxOut {
                value: STANDARD_DENOMINATIONS[4],
                script_pubkey: p2pkh(0x50u8.wrapping_add(i as u8)).into(),
            })
            .collect(),
        0,
    );
    let mut dstx = DsTx {
        tx,
        pro_tx_hash: TxHash::from_lendian(SEED_PRO_TX),
        sig: Vec::new(),
        sig_time: SEED_TIME,
    };
    dstx.sig = sign(&dstx_signature_hash(&dstx));
    dstx
}

fn sign(hash: &dash_num::Hash256) -> Vec<u8> {
    operator_key(0).sign_with(hash.as_bytes(), dash_pkc::bls::BlsSigId::Basic).to_bytes().to_vec()
}

/// Balanced broadcast sizes at the pool limits: the most inputs a pre-V24 pool holds and one
/// more, then the post-V24 most and one more.
pub const POOL_SIZE_BOUNDARIES: [u16; 4] = [180, 181, 200, 201];

/// `diff_coinjoin`'s seeds: each rule at every pick, valid and invalid entries, sessions and
/// collaterals, and signed queues and broadcasts in every message case. Inputs only, judged by
/// the oracle when run.
pub fn seed_inputs() -> Vec<Vec<u8>> {
    let mut seeds = Vec::new();
    // Rules: each at every pick, the other arguments rotating through the picks.
    for (op, &(_, arity)) in RULES.iter().enumerate() {
        for pick in 0..PICKS.len() as u8 {
            let mut w = Writer::case(CASE_RULE).u8(op as u8);
            for k in 0..arity as u8 {
                w = w.u8(2 * ((pick + 7 * k) % PICKS.len() as u8));
            }
            seeds.push(w.0);
        }
    }

    // Entries at every session denomination: a promotion (ten coins into one of the next
    // larger denomination) and a demotion (one into ten), each valid where the session
    // denomination has a larger neighbour.
    let ten: Vec<u8> = (0..10).collect();
    for d in 0..8u8 {
        // DENOMS[d] is bit `bit` for d in 1..=5; its larger neighbour is bit `bit - 1`.
        let bit = d.saturating_sub(1);
        let w = Writer::case(CASE_ENTRY).u8(0).u8(d);
        let larger = [(denomination(bit.saturating_sub(1)), 0x01)];
        seeds.push(finish(w, &ten, &larger));
        let w = Writer::case(CASE_ENTRY).u8(1).u8(d);
        let tens: Vec<(u8, u8)> = (0..10).map(|i| (denomination(bit), 0x10 + i)).collect();
        seeds.push(finish(w, &[0], &tens));
    }

    // Sessions at the smallest denomination (bit 4) on every network.
    let smallest = denomination(4);
    let larger = denomination(3);
    let coins = |n: u8| -> Vec<(u8, u8, u8)> { (0..n).map(|i| (i, smallest, 0x10 + i)).collect() };
    let outs =
        |n: u8, base: u8| -> Vec<(u8, u8)> { (0..n).map(|i| (smallest, base + i)).collect() };
    for network in 0..4u8 {
        let session = |flags: u8| Writer::case(CASE_IN_OUTS).u8(network).u8(flags).u8(denom_bit(4));
        // Standard 3 to 3, with the entry and final-transaction rules.
        for flags in 0..4 {
            let w = write_view(session(flags), &coins(3), &[], 0);
            seeds.push(finish(w, &[0, 1, 2], &outs(3, 0x20)));
        }
        // A promotion and a demotion entry, with and without rebalance shapes.
        for flags in [0, 1] {
            let w = write_view(session(flags), &coins(10), &[], 0);
            seeds.push(finish(w, &ten, &[(larger, 0x30)]));
            let w = write_view(session(flags), &[(0, larger, 0x31)], &[], 0);
            seeds.push(finish(w, &[0], &outs(10, 0x40)));
        }
        // A final transaction holding a promotion and a standard 3 to 3, then the same with
        // one more smallest output (the fees are no longer zero).
        let thirteen: Vec<u8> = (0..13).collect();
        let w = write_view(session(3), &coins(13), &[], 0);
        let mut final_outs = vec![(larger, 0x50)];
        final_outs.extend(outs(3, 0x51));
        seeds.push(finish(w.clone(), &thirteen, &final_outs));
        final_outs.push((smallest, 0x5f));
        seeds.push(finish(w, &thirteen, &final_outs));
        // An input from the mempool, unlocked and locked; a missing input; a null one; a
        // repeated script.
        for mask in [0, 1] {
            let w = write_view(session(0), &[], &[(&[0x70], &[(smallest, 0x60)])], mask);
            seeds.push(finish(w, &[0x80], &outs(1, 0x61)));
        }
        let w = write_view(session(0), &coins(1), &[], 0);
        seeds.push(finish(w, &[0x05], &outs(1, 0x62)));
        let w = write_view(session(0), &coins(1), &[], 0);
        seeds.push(finish(w, &[0xff], &outs(1, 0x63)));
        let w = write_view(session(0), &coins(2), &[], 0);
        seeds.push(finish(w, &[0, 1], &[(smallest, 0x10), (smallest, 0x64)]));
    }

    // Collaterals: valid (the mempool accepting and refusing), a lock time, too small a fee,
    // too large a fee, an unlocked and a locked mempool input, a non-P2PKH output.
    for network in 0..4u8 {
        let collateral = |flags: u8| Writer::case(CASE_COLLATERAL).u8(network).u8(flags);
        let funded = [(0u8, collaterals(2), 0x70u8)];
        for flags in 0..4 {
            let w = write_view(collateral(flags), &funded, &[], 0);
            seeds.push(finish(w, &[0], &[(collaterals(1), 0x71)]));
        }
        let w = write_view(collateral(1), &funded, &[], 0);
        seeds.push(finish(w, &[0], &[(collaterals(2), 0x72)]));
        let w = write_view(collateral(1), &[(0, denomination(0), 0x73)], &[], 0);
        seeds.push(finish(w, &[0], &[(collaterals(1), 0x74)]));
        for mask in [0, 1] {
            let w = write_view(collateral(1), &[], &[(&[0x70], &[(collaterals(2), 0x75)])], mask);
            seeds.push(finish(w, &[0x80], &[(collaterals(1), 0x76)]));
        }
        let w = write_view(collateral(1), &funded, &[], 0);
        seeds.push(finish(w, &[0], &[(collaterals(1), 0xc2)]));
        // The max-fee bound of ATMPIfSaneFee: a fee of exactly MAX_RAW_TX_FEE, and one more.
        for coin in [MAX_RAW_TX_FEE + 10_000, MAX_RAW_TX_FEE + 10_001] {
            seeds.push(finish_raw(collateral(1), &[(0, coin)], &[0], &[(10_000, 0x77)]));
        }
        // dash-coinjoin's own MoneyRange checks (CJ0 D12): an input coin above MAX_MONEY, and a
        // negative output, each refused before the mempool is asked.
        seeds.push(finish_raw(collateral(1), &[(0, MAX_MONEY + 1)], &[0], &[(10_000, 0x77)]));
        seeds.push(finish_raw(collateral(1), &[(0, 20_000)], &[0], &[(-10_000, 0x77), (0, 0x78)]));
    }

    // Sessions at denomination 0, which the oracle refuses (no session has it): unchecked
    // amounts, one an output of i64::MIN, and a value-preserving non-denominated entry.
    for network in 0..4u8 {
        let session = || Writer::case(CASE_IN_OUTS).u8(network).u8(0).u8(0);
        seeds.push(finish_raw(session(), &[(0, 30_000)], &[0], &[(i64::MIN, 0x20)]));
        seeds.push(finish_raw(session(), &[(0, 12_345)], &[0], &[(12_345, 0x20)]));
    }

    // Messages: a signed queue (ready or not) and signed broadcasts (3 to 3, ten to one).
    let dsqs = [wire(&seed_dsq(false)), wire(&seed_dsq(true))];
    let dstxs = [wire(&seed_dstx(3, 3)), wire(&seed_dstx(10, 1)), wire(&seed_dstx(1, 10))];
    for dsq in &dsqs {
        seeds.push(Writer::case(CASE_HASHES).u8(0).bytes(dsq).0);
        for flags in [0, 2] {
            seeds.push(Writer::case(CASE_SIGNATURE).u8(flags).u8(0).bytes(dsq).0);
            seeds.push(Writer::case(CASE_SIGNATURE).u8(flags).u8(1).bytes(dsq).0);
        }
        for delta in [-31i8, -30, 0, 30, 31] {
            seeds.push(Writer::case(CASE_QUEUE_TIME).u8(((delta << 1) as u8) | 1).bytes(dsq).0);
        }
        seeds.push(Writer::case(CASE_QUEUE_TIME).u8(0).int_raw(-1).bytes(dsq).0);
    }
    // The pool-size boundaries, balanced: 180 and 181 (the pre-V24 limit, 20 participants of 9
    // inputs), 200 and 201 (the post-V24 one, 20 of 10), V24 defined and active, at a tip that
    // ends no period on any network.
    for n in POOL_SIZE_BOUNDARIES {
        let dstx = wire(&seed_dstx(n, n));
        for network in 0..4u8 {
            for state in [0u8, 3] {
                let w = Writer::case(CASE_DSTX_STRUCTURE).u8(network).u8(state);
                seeds.push(w.bytes(&4030u16.to_le_bytes()).bytes(&dstx).0);
            }
        }
    }
    for dstx in &dstxs {
        seeds.push(Writer::case(CASE_HASHES).u8(1).bytes(dstx).0);
        for flags in [1, 3] {
            seeds.push(Writer::case(CASE_SIGNATURE).u8(flags).u8(0).bytes(dstx).0);
        }
        for network in 0..4u8 {
            for state in 0..5u8 {
                for height in [99u16, 100, 119, 249, 4031] {
                    let w = Writer::case(CASE_DSTX_STRUCTURE).u8(network).u8(state);
                    seeds.push(w.bytes(&height.to_le_bytes()).bytes(dstx).0);
                }
            }
        }
    }
    seeds
}
