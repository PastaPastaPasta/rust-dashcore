//! EV3-2's `diff_credit_pool`: a block's asset locks and unlocks against the parent's credit pool
//! (`CheckCreditPoolDiffForBlock`, `GetCreditPoolDiffForBlock`) and the pool after it
//! (`GetCreditPool`: `GetCreditDataFromBlock`, `ConstructCreditPool`) against
//! `oracle_credit_pool_step` (v24; the entry dates from ABI 18, the pin is ABI 29).
//!
//! An input starts from a credit step of O2a-GV's payments fixture
//! (`dash-oracle-sys/tests/data/payments-v24.0.json.gz`: mainnet's V20 activation block from the
//! empty pool, and a testnet block with asset unlocks and the platform share) and changes it: the
//! parent's pool (its amounts and its used indexes, with CONS-10's top indexes), the block's
//! transactions (removed, repeated, a crafted asset lock), each asset unlock's index, fee, outputs
//! and payload, each asset lock's `OP_RETURN` output, the CbTx balance and the coinbase's type,
//! and V24's state; and amounts at `int64`'s ends, where rc.4 checks the sums (CE4-05). The
//! block's merkle root follows its transactions. Compared: the verdict, the reject reason, class
//! and debug message, whether the check ran (a CbTx coinbase), the balance the CbTx must carry,
//! and the pool after the block, or the exception Core's construction throws.
//!
//! The node's side is `dash_dml::creditpool`: [`check_block_diff`] and
//! [`credit_pool_diff_for_block`] on the parent's pool, then [`connect_records`] on the window
//! start's block, as the chainstate connects it (EV3 D8, D9).

use std::sync::{Arc, OnceLock};

use dash_consensus::deployments::bip9_enabled;
use dash_consensus::primitives::core_serialize;
use dash_consensus::subsidy::{COIN, block_subsidy_inner};
use dash_consensus::{
    Amount, Block, BlockValidationResult, Header, MAX_MONEY, Tx, TxRef, decode_block,
};
use dash_dml::creditpool::{
    ConnectAt, CreditBlockData, CreditPool, PoolEras, PoolState, RangesSet, check_block_diff,
    connect_records, credit_block_data, credit_pool_diff_for_block, platform_reward,
};
use dash_oracle_sys::{
    Checkpoints, CoinsView, Context, CreditPool as CorePool, CreditPoolStep, DEPLOYMENT_V24,
    ErrorKind, MnList as CoreList, Oracle, Pin, ResultCode, StepContext, ThresholdState,
    UnlockIndexes, Verdict,
};
use dash_params::DeploymentPos;
use dash_primitives::{
    BlockHeader, CoinbaseCommitment, OutPoint, SpecialPayload, Transaction, TxHash, TxIn, TxOut,
    TxType,
};
use dash_types::codec::BaseCodec;
use serde_json::Value;

use crate::dml::unhex;
use crate::payments::{Net, fixture, nets};
use crate::{Compared, Divergence, Outcome, check, divergence};

/// The most edits an input makes.
pub const MAX_OPS: usize = 16;

/// `BlockValidationResult::BLOCK_CONSENSUS`.
const BLOCK_CONSENSUS: ResultCode = ResultCode::Block(1);

/// A credit step of the fixture: a mined block, the headers below it back to the window start,
/// the window start's block and `getcreditpoolinfo`'s pool after the parent.
pub struct Step {
    pub name: String,
    pub net: &'static Net,
    /// The block's height.
    pub height: i32,
    /// The headers of `height - CreditPoolPeriodBlocks() .. height`.
    headers: Vec<[u8; 80]>,
    /// The window start's block, as the node's disk holds it.
    window_block: Vec<u8>,
    header: BlockHeader,
    txs: Vec<Transaction>,
    /// The pool after the parent (`getcreditpoolinfo` has no indexes; none are used before).
    prev: CreditPool,
    /// `getcreditpoolinfo`'s pool after the block: `(locked, current_limit, lately_unlocked)`.
    pub expected: (Amount, Amount, Amount),
    deployments: Vec<(u8, ThresholdState)>,
}

fn pool_of(p: &Value) -> Option<(Amount, Amount, Amount)> {
    Some((p["locked"].as_i64()?, p["current_limit"].as_i64()?, p["lately_unlocked"].as_i64()?))
}

impl Step {
    fn load(s: &Value) -> Option<Step> {
        let net = match s["chain"].as_str()? {
            "main" => &nets()[0],
            "test" => &nets()[1],
            _ => return None,
        };
        let bytes = unhex(s["block"].as_str()?)?;
        let block = decode_block(&mut bytes.as_slice()).ok()?.into_value();
        let headers = unhex(s["headers"].as_str()?)?
            .chunks(80)
            .map(|h| h.try_into().ok())
            .collect::<Option<Vec<[u8; 80]>>>()?;
        let prev = match &s["prev"] {
            Value::Null => CreditPool::default(),
            p => {
                let (locked, current_limit, lately_unlocked) = pool_of(p)?;
                CreditPool {
                    locked,
                    current_limit,
                    lately_unlocked,
                    indexes: RangesSet::default(),
                }
            }
        };
        let deployments = s["deployments"]
            .as_array()?
            .iter()
            .map(|d| {
                let state = u8::try_from(d[1].as_u64()?).ok()?;
                Some((u8::try_from(d[0].as_u64()?).ok()?, ThresholdState::try_from(state).ok()?))
            })
            .collect::<Option<_>>()?;
        Some(Step {
            name: s["name"].as_str()?.to_owned(),
            net,
            height: i32::try_from(s["height"].as_i64()?).ok()?,
            headers,
            window_block: unhex(s["window_block"].as_str()?)?,
            header: *block.header().raw(),
            txs: block.txs().iter().map(|tx| tx.to_mutable()).collect(),
            prev,
            expected: pool_of(&s["expected"])?,
            deployments,
        })
    }

    /// The headers' first height: the window start.
    fn first_height(&self) -> i32 {
        self.height - self.headers.len() as i32
    }

    /// The parent's `nBits`.
    fn prev_bits(&self) -> u32 {
        self.headers.last().map_or(0, |h| u32::from_le_bytes([h[72], h[73], h[74], h[75]]))
    }
}

/// The fixture's credit steps.
#[allow(clippy::expect_used)] // the committed fixture
pub fn steps() -> &'static [Step] {
    static STEPS: OnceLock<Vec<Step>> = OnceLock::new();
    STEPS.get_or_init(|| {
        fixture()["credit"]
            .as_array()
            .expect("credit steps")
            .iter()
            .map(|s| Step::load(s).expect("a credit step"))
            .collect()
    })
}

/// The index of the step named `name`.
pub fn step_index(name: &str) -> Option<usize> {
    steps().iter().position(|s| s.name == name)
}

/// A step as an input edits it.
#[derive(Clone, Debug)]
pub struct Case {
    pub step: usize,
    /// V24 active after the parent (Core ignores it on mainnet, where V24 is never active).
    pub v24: bool,
    pub prev: CreditPool,
    pub txs: Vec<Transaction>,
}

impl Case {
    /// The mined step.
    pub fn mined(step: usize) -> Case {
        let s = &steps()[step];
        Case {
            step,
            v24: false,
            prev: s.prev.clone(),
            txs: s.txs.clone(),
        }
    }

    pub fn step(&self) -> &'static Step {
        &steps()[self.step]
    }

    /// The block, its merkle root recomputed from its transactions.
    pub fn block(&self) -> Block {
        let txs: Vec<TxRef> = self.txs.iter().map(|tx| Arc::new(Tx::new(tx.clone()))).collect();
        let mut header = self.step().header;
        header.merkle_root = Block::new(Header::new(header), txs.clone()).merkle().0;
        Block::new(Header::new(header), txs)
    }

    /// The oracle's context: the headers back to the window start and V24's state.
    fn context(&self) -> Context {
        let s = self.step();
        let mut deployments = s.deployments.clone();
        if self.v24 {
            deployments.retain(|(pos, _)| *pos != DEPLOYMENT_V24);
            deployments.push((DEPLOYMENT_V24, ThresholdState::Active));
        }
        Context::new(
            s.first_height(),
            s.headers.clone(),
            false,
            deployments,
            0,
            Checkpoints::Disabled,
            CoinsView::Unchecked(vec![]),
            CoreList::Absent,
            vec![],
        )
    }

    fn step_context(&self) -> StepContext {
        StepContext {
            prev: CorePool {
                locked: self.prev.locked,
                current_limit: self.prev.current_limit,
                lately_unlocked: self.prev.lately_unlocked,
                indexes: UnlockIndexes {
                    ranges: self.prev.indexes.ranges().collect(),
                },
            },
            blocks: vec![self.step().window_block.clone()],
        }
    }
}

/// The node's answers.
#[derive(Debug)]
struct Ours {
    /// The coinbase is a CbTx: `ProcessSpecialTxsInBlock` runs the check.
    checked: bool,
    /// The balance the CbTx must carry.
    expected: Option<Amount>,
    /// The check's reject.
    reject: Option<dash_consensus::BlockValidationError>,
    /// The pool after the block (or the exception), when the check accepts.
    pool: Option<PoolState>,
}

/// What the deployments and the subsidy make of the case.
struct At {
    dip8: bool,
    v20: bool,
    connect: ConnectAt,
    platform_reward: Amount,
}

fn at(case: &Case) -> Result<At, Divergence> {
    let s = case.step();
    let c = &s.net.params.consensus;
    let h = s.height;
    let active = |height: u32| i64::from(h) >= i64::from(height);
    let v20 = active(c.v20_height.to_u32());
    let subsidy = block_subsidy_inner(s.prev_bits(), h - 1, v20, s.net.payments.subsidy())
        .map_err(|_| divergence("node fault"))?;
    // `DeploymentActiveAfter(pindexPrev, MN_RR)`: active at the block.
    let platform_reward =
        platform_reward(active(c.mn_rr_height.to_u32()), h, subsidy, &s.net.payment)
            .map_err(|_| divergence("node fault"))?;
    Ok(At {
        dip8: active(c.dip0008_height.to_u32()),
        v20,
        connect: ConnectAt {
            dip3: active(c.dip0003_height.to_u32()),
            v20,
            eras: PoolEras {
                v24: case.v24 && bip9_enabled(s.net.params, DeploymentPos::V24),
                withdrawals: active(c.withdrawals_height.to_u32()),
            },
        },
        platform_reward,
    })
}

/// The CbTx balance `ProcessSpecialTxsInBlock` reads, when the coinbase is of type
/// `TRANSACTION_COINBASE` (Core reads the type whatever the version; the oracle refuses a CbTx
/// payload that does not decode).
fn cbtx_balance(block: &Block) -> Result<Option<Amount>, Divergence> {
    let coinbase = &block.txs()[0];
    if coinbase.tx_type() != TxType::CoinbaseCommitment {
        return Ok(None);
    }
    match coinbase.payload() {
        Some(Ok(decoded)) if decoded.is_complete() => match &decoded.payload {
            SpecialPayload::CoinbaseCommitment(cbtx) => {
                Ok(Some(cbtx.credit_pool_balance.unwrap_or(0)))
            }
            _ => Err(divergence("CbTx payload")),
        },
        _ => Err(divergence("CbTx payload")),
    }
}

fn ours(case: &Case, block: &Block) -> Result<Ours, Divergence> {
    let s = case.step();
    let at = at(case)?;
    let cbtx = cbtx_balance(block)?;
    let expected = if at.dip8 && at.v20 {
        credit_pool_diff_for_block(block, &case.prev, at.platform_reward)
            .ok()
            .and_then(|d| d.total_locked())
    } else {
        None
    };
    let reject = cbtx.and_then(|balance| {
        check_block_diff(block, balance, &case.prev, at.platform_reward, at.dip8, at.v20).err()
    });
    let pool = if reject.is_some() {
        None
    } else {
        // The window start's data, as the node stored it when that block connected.
        let c = &s.net.params.consensus;
        let window = decode_block(&mut s.window_block.as_slice())
            .map_err(|_| divergence("window block"))?
            .into_value();
        let window_dip3 = i64::from(s.first_height()) >= i64::from(c.dip0003_height.to_u32());
        // The oracle refuses a window block Core cannot read (CheckDiskUnlocks).
        let window_data = credit_block_data(&window, window_dip3)
            .map_err(|_| divergence("window block"))?
            .map(|credit| credit.data);
        let records = connect_records(block, at.connect, &case.prev, window_data.as_ref());
        Some(records.pool.unwrap_or_else(|| Ok(CreditPool::default())))
    };
    Ok(Ours {
        checked: cbtx.is_some(),
        expected,
        reject,
        pool,
    })
}

fn same_pool(core: &CorePool, ours: &CreditPool) -> bool {
    core.locked == ours.locked
        && core.current_limit == ours.current_limit
        && core.lately_unlocked == ours.lately_unlocked
        && core.indexes.ranges.iter().copied().eq(ours.indexes.ranges())
}

/// Core's step on `case`; `Err` with Core's exception text where `GetCreditPool` throws; `None`
/// where the oracle refuses the call.
pub fn core_step(case: &Case) -> Result<Option<Result<CreditPoolStep, String>>, Divergence> {
    let block = core_serialize(&case.block());
    match Oracle::new(Pin::V24_0).credit_pool_step(
        &(case.step().net.chain)(),
        &block,
        &case.context(),
        &case.step_context(),
    ) {
        Ok(step) => Ok(Some(Ok(step))),
        Err(e) if e.kind == ErrorKind::Exception => Ok(Some(Err(e.message))),
        Err(e)
            if matches!(e.kind, ErrorKind::Context | ErrorKind::Argument | ErrorKind::Decode) =>
        {
            Ok(None)
        }
        Err(_) => Err(divergence("oracle error")),
    }
}

/// One case on both sides.
pub fn compare_case(case: &Case) -> Compared {
    let Some(core) = core_step(case)? else {
        return Ok(Outcome::OutOfDomain);
    };
    let ours = ours(case, &case.block())?;
    let core = match core {
        Ok(core) => core,
        Err(message) => {
            // `GetCreditPool` threw after the check accepted.
            check(ours.reject.is_none(), "verdict")?;
            return match &ours.pool {
                Some(Err(e)) if message == format!("GetCreditPool: {e}") => Ok(Outcome::BothReject),
                Some(Err(_)) => Err(divergence("pool exception")),
                _ => Err(divergence("pool")),
            };
        }
    };
    match (&core.verdict, &ours.reject) {
        (Verdict::Accept, None) => {}
        (Verdict::Reject(rej), Some(e)) => {
            check(!rej.reason_truncated && rej.reason == e.reject_reason(), "reject reason")?;
            check(
                rej.result == BLOCK_CONSENSUS && e.result() == BlockValidationResult::Consensus,
                "reject result",
            )?;
            check(!rej.debug_truncated && rej.debug_message == e.debug_message(), "debug message")?;
        }
        _ => return Err(divergence("verdict")),
    }
    check(core.checked == ours.checked, "checked")?;
    check(core.expected == ours.expected, "expected balance")?;
    match (&core.pool, &ours.pool) {
        (None, None) => {}
        (Some(c), Some(Ok(o))) => check(same_pool(c, o), "pool")?,
        _ => return Err(divergence("pool")),
    }
    Ok(if ours.reject.is_none() {
        Outcome::BothAccept
    } else {
        Outcome::BothReject
    })
}

/// What an asset unlock releases (`GetDataFromUnlockTx`'s amount, 0 for a payload that does not
/// decode), summed over the block.
fn unlocked_total(txs: &[Transaction]) -> Amount {
    txs.iter()
        .filter(|tx| is_special(tx, TxType::AssetUnlock))
        .map(|tx| {
            let fee = tx
                .extra_payload
                .get(9..13)
                .map_or(0, |f| Amount::from(u32::from_le_bytes([f[0], f[1], f[2], f[3]])));
            tx.outputs.iter().fold(fee, |sum, o| sum.wrapping_add(o.value))
        })
        .fold(0, Amount::wrapping_add)
}

fn is_special(tx: &Transaction, tx_type: TxType) -> bool {
    tx.version >= 3 && tx.tx_type == tx_type
}

/// The non-coinbase transactions of `tx_type` (all of them for `None`), by position.
fn positions(case: &Case, tx_type: Option<TxType>) -> Vec<usize> {
    (1..case.txs.len()).filter(|&i| tx_type.is_none_or(|t| is_special(&case.txs[i], t))).collect()
}

/// An asset unlock's index (its payload's bytes 1..9).
fn unlock_index(tx: &Transaction) -> Option<u64> {
    tx.extra_payload.get(1..9).map(|b| u64::from_le_bytes(b.try_into().unwrap_or_default()))
}

fn set_unlock_index(tx: &mut Transaction, index: u64) {
    if let Some(bytes) = tx.extra_payload.get_mut(1..9) {
        bytes.copy_from_slice(&index.to_le_bytes());
    }
}

/// Edits the CbTx's decoded payload; a coinbase without one is left as it is.
fn edit_cbtx(case: &mut Case, f: impl FnOnce(&mut CoinbaseCommitment)) {
    let coinbase = &mut case.txs[0];
    let mut payload = coinbase.extra_payload.as_slice();
    let Ok(mut cbtx) = CoinbaseCommitment::decode(&mut payload) else {
        return;
    };
    let rest = payload.to_vec();
    f(&mut cbtx);
    let mut bytes = Vec::new();
    cbtx.encode(&mut bytes);
    bytes.extend(rest);
    coinbase.extra_payload = bytes;
}

/// The script an asset lock's credit output and a crafted output pay to.
const P2PKH: [u8; 25] =
    [0x76, 0xa9, 0x14, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 0x88, 0xac];

/// An asset lock of `amount`: one credit output, the `OP_RETURN` output that burns it.
fn asset_lock(amount: Amount, tag: u8) -> Transaction {
    let mut payload = vec![1, 1];
    payload.extend(amount.to_le_bytes());
    payload.push(P2PKH.len() as u8);
    payload.extend(P2PKH);
    Transaction {
        version: 3,
        tx_type: TxType::AssetLock,
        inputs: vec![TxIn {
            prevout: OutPoint {
                hash: TxHash::from([tag; 32]),
                index: 0,
            },
            script_sig: Default::default(),
            sequence: u32::MAX,
        }],
        outputs: vec![TxOut {
            value: amount,
            script_pubkey: vec![0x6a, 0].into(),
        }],
        lock_time: 0,
        extra_payload: payload,
    }
}

/// The case an input describes: `[step, flags, (op, a, b)*]`.
///
/// Flags: V24 active after the parent (bit 0); after the edits, the CbTx balance set to the one
/// the node expects, so the pool is compared (bit 1).
pub fn case_from_bytes(input: &[u8]) -> Option<Case> {
    let (&[selector, flags], ops) = input.split_first_chunk::<2>()?;
    let mut case = Case::mined(usize::from(selector) % steps().len());
    case.v24 = flags & 1 != 0;
    for &[op, a, b] in ops.as_chunks::<3>().0.iter().take(MAX_OPS) {
        edit(&mut case, op, a, b);
    }
    if flags & 2 != 0 {
        rebalance(&mut case);
    }
    Some(case)
}

/// Sets the CbTx balance to the total the node computes for the block, when it computes one.
fn rebalance(case: &mut Case) {
    let Ok(at) = at(case) else {
        return;
    };
    let Ok(diff) = credit_pool_diff_for_block(&case.block(), &case.prev, at.platform_reward) else {
        return;
    };
    let Some(total) = diff.total_locked() else {
        return;
    };
    edit_cbtx(case, |cbtx| {
        if cbtx.credit_pool_balance.is_some() {
            cbtx.credit_pool_balance = Some(total);
        }
    });
}

/// What an asset lock or unlock sums across this many transactions of `MAX_MONEY` each: at 4392
/// (and an unlock's fee) it stays within `int64`, at 4393 it overflows, which rc.4 checks
/// (CE4-05; `CheckTransaction` keeps a single transaction within the money range).
const MAX_MONEY_TXS: usize = 4392;

/// Applies one edit; an edit that does not apply to the case does nothing.
fn edit(case: &mut Case, op: u8, a: u8, b: u8) {
    // Bit 7 of `a` scales the amount deltas to whole coins; bit 6 picks the edits that reach
    // `int64`'s ends (CE4-05).
    let extreme = a & 0x40 != 0;
    let delta = (Amount::from(b) + 1)
        * if a & 0x80 != 0 {
            COIN
        } else {
            1
        };
    let unlocks = positions(case, Some(TxType::AssetUnlock));
    let pick = |list: &[usize]| (!list.is_empty()).then(|| list[usize::from(a) % list.len()]);
    let prev = &mut case.prev;
    match op % 16 {
        0 if extreme => prev.locked = Amount::MAX - (delta - 1),
        1 if extreme => prev.locked = Amount::MIN + (delta - 1),
        0 => prev.locked = prev.locked.wrapping_add(delta),
        1 => prev.locked = prev.locked.wrapping_sub(delta),
        2 => {
            let total = unlocked_total(&case.txs);
            prev.current_limit = match b % 4 {
                0 => 0,
                1 => total - 1,
                2 => total,
                _ if extreme => Amount::MAX,
                _ => prev.current_limit.wrapping_add(delta),
            };
        }
        3 => {
            prev.lately_unlocked = match b % 4 {
                0 => 0,
                1 => prev.lately_unlocked.wrapping_add(delta),
                2 => prev.lately_unlocked.wrapping_sub(delta),
                _ => 1000 * COIN + Amount::from(a & 0x7f),
            };
        }
        // A used index: one of the block's, or one near it.
        4 => {
            let index = pick(&unlocks).and_then(|i| unlock_index(&case.txs[i]));
            prev.indexes.add(index.unwrap_or(u64::from(a)));
        }
        // CONS-10: the top indexes, whose range wraps, and the neighbours of the block's.
        5 => {
            let index = unlocks.first().and_then(|&i| unlock_index(&case.txs[i])).unwrap_or(0);
            prev.indexes.add(match b % 5 {
                0 => u64::from(a),
                1 => u64::MAX,
                2 => u64::MAX - 1,
                3 => index.wrapping_sub(1),
                _ => index.wrapping_add(1),
            });
        }
        6 => {
            if let Some(i) = pick(&positions(case, None)) {
                case.txs.remove(i);
            }
        }
        // An asset unlock paying `MAX_MONEY`, then that many copies of it under fresh indexes
        // (b odd: one more, whose sum overflows).
        7 if extreme => {
            if let Some(i) = pick(&unlocks) {
                let tx = &mut case.txs[i];
                tx.outputs = vec![TxOut {
                    value: MAX_MONEY,
                    script_pubkey: P2PKH.to_vec().into(),
                }];
                let base = u64::MAX / 2;
                let copies: Vec<Transaction> = (0..MAX_MONEY_TXS - 1 + usize::from(b & 1))
                    .map(|k| {
                        let mut copy = tx.clone();
                        set_unlock_index(&mut copy, base + k as u64);
                        copy
                    })
                    .collect();
                case.txs.splice(i + 1..i + 1, copies);
            }
        }
        // A transaction twice (a mutated merkle tree, which the oracle refuses), or an asset
        // unlock's index twice: a copy with another quorum hash byte (b odd).
        7 => {
            let list = if b & 1 == 0 {
                positions(case, None)
            } else {
                unlocks
            };
            if let Some(i) = pick(&list) {
                let mut tx = case.txs[i].clone();
                if b & 1 != 0
                    && let Some(byte) = tx.extra_payload.get_mut(17)
                {
                    *byte ^= 1;
                }
                case.txs.insert(i + 1, tx);
            }
        }
        8 => {
            if let Some(i) = pick(&unlocks) {
                let other = unlocks.iter().rev().find_map(|&j| unlock_index(&case.txs[j]));
                let index = match b % 4 {
                    0 => u64::from(a),
                    1 => other.unwrap_or(0),
                    2 => u64::MAX,
                    _ => 0,
                };
                set_unlock_index(&mut case.txs[i], index);
            }
        }
        9 => {
            if let Some(out) = pick(&unlocks).and_then(|i| case.txs[i].outputs.first_mut()) {
                out.value = if b & 1 == 0 {
                    out.value + delta
                } else {
                    out.value - delta
                };
            }
        }
        10 => edit_cbtx(case, |cbtx| {
            if let Some(balance) = cbtx.credit_pool_balance.as_mut() {
                *balance = if b & 1 == 0 {
                    *balance + delta
                } else {
                    *balance - delta
                };
            }
        }),
        11 => {
            if let Some(i) = pick(&positions(case, Some(TxType::AssetLock))) {
                let outputs = &mut case.txs[i].outputs;
                let burn =
                    outputs.iter().position(|o| o.script_pubkey.as_bytes().first() == Some(&0x6a));
                match (burn, b % 3) {
                    (Some(j), 0) => outputs[j].value += delta,
                    (Some(j), 1) => {
                        outputs.remove(j);
                    }
                    (Some(j), _) => outputs[j].script_pubkey = Vec::new().into(),
                    (None, _) => {}
                }
            }
        }
        // An unlock's fee, or its payload version 2: V24's stable txid, the same layout
        // (`assetlocktx.h:79-83`), so the reject's text names another hash.
        12 => {
            if let Some(i) = pick(&unlocks) {
                let payload = &mut case.txs[i].extra_payload;
                let fee = match b % 4 {
                    0 => 0,
                    1 => u32::from(a),
                    2 => u32::MAX,
                    _ => {
                        if let Some(version) = payload.first_mut() {
                            *version = 2;
                        }
                        return;
                    }
                };
                if let Some(bytes) = payload.get_mut(9..13) {
                    bytes.copy_from_slice(&fee.to_le_bytes());
                }
            }
        }
        // A payload `GetTxPayload` refuses: a byte short or a byte over.
        13 => {
            let specials: Vec<usize> = positions(case, None)
                .into_iter()
                .filter(|&i| case.txs[i].version >= 3 && !case.txs[i].extra_payload.is_empty())
                .collect();
            if let Some(i) = pick(&specials) {
                let payload = &mut case.txs[i].extra_payload;
                if b & 1 == 0 {
                    payload.pop();
                } else {
                    payload.push(0);
                }
            }
        }
        // The coinbase without a CbTx: a plain coinbase, another special type with the CbTx's
        // bytes (Core reads the balance of any special-version coinbase), a version-2 CbTx.
        14 => match b % 3 {
            0 => {
                let coinbase = &mut case.txs[0];
                coinbase.version = 2;
                coinbase.tx_type = TxType::new(0);
                coinbase.extra_payload.clear();
            }
            1 => case.txs[0].tx_type = TxType::new(1),
            _ => edit_cbtx(case, |cbtx| {
                cbtx.version = 2;
                cbtx.best_cl_height_diff = None;
                cbtx.best_cl_signature = None;
                cbtx.credit_pool_balance = None;
                cbtx.merkle_root_asset_unlocks = None;
            }),
        },
        // Asset locks of `MAX_MONEY` (b odd: one more, whose sum overflows).
        _ if extreme => {
            for k in 0..MAX_MONEY_TXS + usize::from(b & 1) {
                let mut lock = asset_lock(MAX_MONEY, a);
                lock.inputs[0].prevout.index = k as u32;
                case.txs.push(lock);
            }
        }
        _ => case.txs.push(asset_lock(delta, a)),
    }
}

/// `diff_credit_pool` (see [`case_from_bytes`]).
pub fn compare_credit_pool(input: &[u8], pin: Pin) -> Compared {
    if pin != Pin::V24_0 {
        return Ok(Outcome::OutOfDomain);
    }
    match case_from_bytes(input) {
        Some(case) => compare_case(&case),
        None => Ok(Outcome::OutOfDomain),
    }
}

/// The target's start: the fixture, read once.
pub fn start(_: Pin) {
    let _ = steps();
}

/// The target's public seeds: each step in each flag state, and every edit at a few arguments
/// (whole-coin deltas among them), with and without the rebalanced CbTx. Call bytes only, judged
/// by the oracle when run.
pub fn seed_inputs() -> Vec<Vec<u8>> {
    let mut seeds = Vec::new();
    for step in 0..steps().len() as u8 {
        for flags in 0..4u8 {
            seeds.push(vec![step, flags]);
            for op in 0..16u8 {
                for (a, b) in [(0, 0), (1, 1), (2, 2), (0x81, 3), (5, 4)] {
                    seeds.push(vec![step, flags, op, a, b]);
                }
            }
        }
        // Two used top indexes merge into one wrapped range (CONS-10), then the block's own.
        seeds.push(vec![step, 2, 5, 0, 1, 5, 0, 2, 4, 0, 0]);
        // A version-2 unlock whose index is used: the reject names its stable txid.
        seeds.push(vec![step, 2, 12, 0, 3, 4, 0, 0]);
        // CE4-05: the total past `int64`'s top and bottom, and the session's locks and unlocks
        // overflowing (under the widest limit).
        seeds.push(vec![step, 0, 0, 0x40, 0, 15, 0, 0]);
        seeds.push(vec![step, 0, 1, 0x40, 0]);
        seeds.push(vec![step, 0, 15, 0x40, 1]);
        seeds.push(vec![step, 0, 2, 0x40, 3, 7, 0x40, 1]);
    }
    seeds
}

/// Each step's data as the node stores it for the window start (D9), for tests.
pub fn window_data(step: usize) -> Option<CreditBlockData> {
    let s = &steps()[step];
    let window = decode_block(&mut s.window_block.as_slice()).ok()?.into_value();
    credit_block_data(&window, true).ok()?.map(|c| c.data)
}
