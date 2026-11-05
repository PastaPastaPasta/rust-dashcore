//! A block's defect instances, measured from its bytes and chain-only coin facts (O10 plan §4.3):
//! each row with an instance, and its trigger (the transaction index, and the input or output
//! index for rows inside CheckTransaction's, CheckTxInputs' or CheckInputScripts' input and output
//! loops), never from what a variant declares. A cumulative row's trigger is the first index
//! where the running sum crosses the bound; a row outside every loop has none. Core's own code
//! decides where the oracle has it: CheckTransaction's row (its loop row's index is read off the
//! bytes), the merkle root and its mutation, proof of work, sigop counts and VerifyScript. Every
//! other rule is read off the bytes, with its Core site.
//!
//! The measured iteration facts (`coinbase`, `special_version`, `has_payload`, `cb_type5_at`)
//! and the domain lengths come with the instances: the walk reads the block as it is.

use std::collections::{BTreeMap, BTreeSet};

use dash_consensus::coins::is_unspendable;
use dash_consensus::primitives::core_serialize;
use dash_oracle_sys::{Chain, Oracle, Pin, SpentOutput, Verdict};

use super::ledger::{Ledger, LedgerCoin, is_coinbase};
use super::precedence::{At, Facts};
use crate::chain::build::{BlockPlan, Built, TxPlan, script_num, tx_size};
use crate::chain::mutate::{
    BIP147_HEIGHT, CLTV_HEIGHT, CSV_HEIGHT, DERSIG_HEIGHT, DIP0001_HEIGHT, MAX_MONEY,
    SEQUENCE_DISABLE, block_size,
};
use crate::chain::world::Outpoint;

pub const CBH_POW: &str = "validation.cpp:CheckBlockHeader#1:high-hash";
pub const CCBH_BITS: &str = "validation.cpp:ContextualCheckBlockHeader#2:bad-diffbits";
pub const CCBH_OLD: &str = "validation.cpp:ContextualCheckBlockHeader#4:time-too-old";
pub const CCBH_NEW: &str = "validation.cpp:ContextualCheckBlockHeader#5:time-too-new";
pub const CCBH_VERSION: &str = "validation.cpp:ContextualCheckBlockHeader#6:bad-version(0x%08x)";
pub const CMR_ROOT: &str = "validation.cpp:CheckMerkleRoot#1:bad-txnmrklroot";
pub const CMR_DUP: &str = "validation.cpp:CheckMerkleRoot#2:bad-txns-duplicate";
pub const CB_LENGTH: &str = "validation.cpp:CheckBlock#1:bad-blk-length";
pub const CB_MULTIPLE: &str = "validation.cpp:CheckBlock#3:bad-cb-multiple";
pub const CB_SIGOPS: &str = "validation.cpp:CheckBlock#4:bad-blk-sigops";
pub const CCB_LENGTH: &str = "validation.cpp:ContextualCheckBlock#1:bad-blk-length";
pub const CCB_FINAL: &str = "validation.cpp:ContextualCheckBlock#2:bad-txns-nonfinal";
pub const CCB_SIGOPS: &str = "validation.cpp:ContextualCheckBlock#3:bad-blk-sigops";
pub const CCB_HEIGHT: &str = "validation.cpp:ContextualCheckBlock#4:bad-cb-height";
pub const CCT_OVERSIZE: &str = "validation.cpp:ContextualCheckTransaction#5:bad-txns-oversize";
pub const CONN_BIP30: &str = "validation.cpp:ConnectBlock#2:bad-txns-BIP30";
pub const CONN_FEES: &str = "validation.cpp:ConnectBlock#4:bad-txns-accumulated-fee-outofrange";
pub const CONN_BIP68: &str = "validation.cpp:ConnectBlock#5:bad-txns-nonfinal";
pub const CONN_SIGOPS: &str = "validation.cpp:ConnectBlock#6:bad-blk-sigops";
pub const CONN_AMOUNT: &str = "validation.cpp:ConnectBlock#9:bad-cb-amount";
pub const CIS_SCRIPT: &str =
    "validation.cpp:CheckInputScripts#2:mandatory-script-verify-flag-failed (%s)";
pub const CTI_MISSING: &str =
    "tx_verify.cpp:Consensus::CheckTxInputs#1:bad-txns-inputs-missingorspent";
pub const CTI_PREMATURE: &str =
    "tx_verify.cpp:Consensus::CheckTxInputs#2:bad-txns-premature-spend-of-coinbase";
pub const CTI_VALUES: &str =
    "tx_verify.cpp:Consensus::CheckTxInputs#3:bad-txns-inputvalues-outofrange";
pub const CTI_BELOW: &str = "tx_verify.cpp:Consensus::CheckTxInputs#4:bad-txns-in-belowout";
pub const STX_DIP3: &str = "specialtxman.cpp:CheckSpecialTxInner#1:bad-tx-type-dip3-inactive";
pub const STX_CB_PAYLOAD: &str = "specialtxman.cpp:ProcessSpecialTxsInBlock#2:bad-cbtx-payload";
/// CheckTransaction's rows, by reject reason (tx_check.cpp:11-58, in order).
pub const CHECK_TRANSACTION: [&str; 10] = [
    "tx_check.cpp:CheckTransaction#1:bad-txns-vin-empty",
    "tx_check.cpp:CheckTransaction#2:bad-txns-vout-empty",
    "tx_check.cpp:CheckTransaction#3:bad-txns-oversize",
    "tx_check.cpp:CheckTransaction#4:bad-txns-payload-oversize",
    "tx_check.cpp:CheckTransaction#5:bad-txns-vout-negative",
    "tx_check.cpp:CheckTransaction#6:bad-txns-vout-toolarge",
    "tx_check.cpp:CheckTransaction#7:bad-txns-txouttotal-toolarge",
    "tx_check.cpp:CheckTransaction#8:bad-txns-inputs-duplicate",
    "tx_check.cpp:CheckTransaction#9:bad-cb-length",
    "tx_check.cpp:CheckTransaction#10:bad-txns-prevout-null",
];
/// CheckTransaction's rows inside its output and input loops (tx_check.cpp:39-58, 69-71).
const LOOP_ROWS: [&str; 5] = [
    CHECK_TRANSACTION[4],
    CHECK_TRANSACTION[5],
    CHECK_TRANSACTION[6],
    CHECK_TRANSACTION[7],
    CHECK_TRANSACTION[9],
];

/// Every row [`measure`] can find: each a row of the model's Submit and Proposal trees
/// (`rows_are_the_models`), so no measured instance is dropped by `precedence::setting` for
/// want of a row.
pub fn rows() -> BTreeSet<&'static str> {
    [
        CBH_POW,
        CCBH_BITS,
        CCBH_OLD,
        CCBH_NEW,
        CCBH_VERSION,
        CMR_ROOT,
        CMR_DUP,
        CB_LENGTH,
        CB_MULTIPLE,
        CB_SIGOPS,
        CCB_LENGTH,
        CCB_FINAL,
        CCB_SIGOPS,
        CCB_HEIGHT,
        CCT_OVERSIZE,
        CONN_BIP30,
        CONN_FEES,
        CONN_BIP68,
        CONN_SIGOPS,
        CONN_AMOUNT,
        CIS_SCRIPT,
        CTI_MISSING,
        CTI_PREMATURE,
        CTI_VALUES,
        CTI_BELOW,
        STX_DIP3,
        STX_CB_PAYLOAD,
    ]
    .into_iter()
    .chain(CHECK_TRANSACTION)
    .collect()
}

/// `MAX_FUTURE_BLOCK_TIME` (chain.h:24).
const MAX_FUTURE: i64 = 2 * 60 * 60;
/// `COINBASE_MATURITY` (consensus.h:19).
const MATURITY: u32 = 100;
/// The coinbase's special type (primitives/transaction.h `TRANSACTION_COINBASE`).
const TRANSACTION_COINBASE: u16 = 5;

/// What the chain says about the block's place: its height and parent, from dashd's template and
/// the ledger (chain-only reads).
pub struct Place<'a> {
    pub height: u32,
    /// The parent's median time past.
    pub mtp: u32,
    /// The node clock (the leg's mocktime).
    pub now: i64,
    /// The template's `nBits`: GetNextWorkRequired's (v:4504).
    pub bits: u32,
    /// The template's coinbase value: the block's subsidy (no fees).
    pub reward: i64,
    /// The UTXO set on the parent, and the times BIP68 reads.
    pub coins: &'a Ledger,
}

/// A block's measured defects and the setting the walk reads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Measured {
    pub defects: Vec<(String, At)>,
    pub facts: Facts,
    /// Each transaction's input and output counts, in block order.
    pub shapes: Vec<(usize, usize)>,
}

impl Measured {
    /// The rows with an instance, each once.
    pub fn rows(&self) -> BTreeSet<&str> {
        self.defects.iter().map(|(r, _)| r.as_str()).collect()
    }

    fn add(&mut self, row: &str, at: At) {
        if !self.defects.iter().any(|(r, a)| r == row && *a == at) {
            self.defects.push((row.to_owned(), at));
        }
    }
}

fn tx_at(i: usize) -> At {
    At {
        vtx: Some(i),
        vin: None,
        vout: None,
    }
}

fn input_at(i: usize, j: usize) -> At {
    At {
        vtx: Some(i),
        vin: Some(j),
        vout: None,
    }
}

fn output_at(i: usize, n: usize) -> At {
    At {
        vtx: Some(i),
        vin: None,
        vout: Some(n),
    }
}

/// CheckTransaction's first loop-row instance in `tx` (tx_check.cpp:39-58, 69-71), read from the
/// bytes: the first output that is negative, over `MAX_MONEY`, or takes the running total out of
/// range (:39-46); else the first input whose prevout an earlier input spends (:55-57); else, in
/// a non-coinbase transaction, the first null prevout (:69-71). `None` when no loop row holds.
pub fn check_transaction_loops(tx: &TxPlan, i: usize) -> Option<(&'static str, At)> {
    let [.., negative, toolarge, total, duplicate, _, null] = CHECK_TRANSACTION;
    let mut value_out = 0i64;
    for (n, (value, _)) in tx.outputs.iter().enumerate() {
        if *value < 0 {
            return Some((negative, output_at(i, n)));
        }
        if *value > MAX_MONEY {
            return Some((toolarge, output_at(i, n)));
        }
        value_out = value_out.saturating_add(*value);
        if !money_range(value_out) {
            return Some((total, output_at(i, n)));
        }
    }
    let mut seen = BTreeSet::new();
    if let Some(j) = tx.inputs.iter().position(|input| !seen.insert(input.prevout)) {
        return Some((duplicate, input_at(i, j)));
    }
    // `IsCoinBase` (transaction.h): one input, its prevout null.
    let is_null = |input: &crate::chain::build::Input| input.prevout == ([0; 32], u32::MAX);
    let coinbase = tx.inputs.len() == 1 && is_null(&tx.inputs[0]);
    tx.inputs.iter().position(is_null).filter(|_| !coinbase).map(|j| (null, input_at(i, j)))
}

fn money_range(v: i64) -> bool {
    (0..=MAX_MONEY).contains(&v)
}

/// The script flags of a block at `height` on L3's legs (`GetBlockScriptFlags`, v:2424-2453:
/// each buried deployment active at the block itself).
fn script_flags(height: u32) -> u32 {
    use dash_consensus::script::ScriptFlags as F;
    let mut flags = F::P2SH;
    for (at, flag) in [
        (DERSIG_HEIGHT, F::DERSIG),
        (CLTV_HEIGHT, F::CHECKLOCKTIMEVERIFY),
        (CSV_HEIGHT, F::CHECKSEQUENCEVERIFY),
        (BIP147_HEIGHT, F::NULLDUMMY),
    ] {
        if height >= at {
            flags = flags.union(flag);
        }
    }
    flags.bits()
}

/// `IsFinalTx` (tx_verify.cpp:17-35).
fn is_final(tx: &TxPlan, height: u32, cutoff: u32) -> bool {
    const LOCKTIME_THRESHOLD: u32 = 500_000_000;
    if tx.lock_time == 0 {
        return true;
    }
    let limit = if tx.lock_time < LOCKTIME_THRESHOLD {
        height
    } else {
        cutoff
    };
    tx.lock_time < limit || tx.inputs.iter().all(|i| i.sequence == u32::MAX)
}

/// Whether `tx`'s relative locks hold at `height` (CalculateSequenceLocks and
/// EvaluateSequenceLocks, tx_verify.cpp:39-104); `None` when a time it needs was never seen.
fn sequence_locks_hold(tx: &TxPlan, heights: &[u32], height: u32, coins: &Ledger) -> Option<bool> {
    const TYPE_FLAG: u32 = 1 << 22;
    const MASK: u32 = 0x0000_ffff;
    const GRANULARITY: u32 = 9;
    // tx_verify.cpp:55-56: the version is read unsigned, so -1 is enforced.
    if (tx.version as i32 as u32) < 2 {
        return Some(true);
    }
    let parent_mtp = coins.mtp(height - 1)?;
    for (input, &coin_height) in tx.inputs.iter().zip(heights) {
        if input.sequence & SEQUENCE_DISABLE != 0 {
            continue;
        }
        let lock = input.sequence & MASK;
        if input.sequence & TYPE_FLAG != 0 {
            let coin_mtp = coins.mtp(coin_height.saturating_sub(1))?;
            if i64::from(coin_mtp) + i64::from(lock << GRANULARITY) > i64::from(parent_mtp) {
                return Some(false);
            }
        } else if coin_height + lock > height {
            return Some(false);
        }
    }
    Some(true)
}

/// `block`'s defect instances at `place` (§4.3), with the walk's setting. `Err` where a rule the
/// block reaches cannot be decided from the bytes and the chain (an undecidable BIP68 time, an
/// oracle error): a harness error, never an absent defect.
/// What a header's rows read: the height, the parent's median time past, the node clock and
/// the `nBits` GetNextWorkRequired gives at that height.
pub struct HeaderPlace {
    pub height: u32,
    pub mtp: u32,
    pub now: i64,
    pub bits: u32,
}

impl HeaderPlace {
    fn of(place: &Place<'_>) -> Self {
        HeaderPlace {
            height: place.height,
            mtp: place.mtp,
            now: place.now,
            bits: place.bits,
        }
    }
}

/// CheckBlockHeader's and ContextualCheckBlockHeader's rows, from the header's bytes.
fn header(
    oracle: &Oracle,
    plan: &BlockPlan,
    built: &Built,
    place: &HeaderPlace,
    m: &mut Measured,
) -> Result<(), String> {
    let h = place.height;
    // CheckBlockHeader (v:4305-4321): proof of work, where the mode checks it.
    if !oracle
        .check_proof_of_work(&Chain::regtest(), &built.hash, plan.bits)
        .map_err(|e| format!("the oracle's CheckProofOfWork: {e:?}"))?
    {
        m.add(CBH_POW, At::default());
    }
    // ContextualCheckBlockHeader (v:4487-4536).
    if plan.bits != place.bits {
        m.add(CCBH_BITS, At::default());
    }
    if plan.time <= place.mtp {
        m.add(CCBH_OLD, At::default());
    }
    if i64::from(plan.time) > place.now + MAX_FUTURE {
        m.add(CCBH_NEW, At::default());
    }
    // v:4525-4527: HEIGHTINCB from height 1 (BIP34Height 1 on regtest), DERSIG, CLTV.
    let v = plan.version;
    if (v < 2 && h >= 1) || (v < 3 && h >= DERSIG_HEIGHT) || (v < 4 && h >= CLTV_HEIGHT) {
        m.add(CCBH_VERSION, At::default());
    }
    Ok(())
}

/// A block's header rows alone (Opus r1 m3: a class 7 pair's last block, whose transactions
/// sit on a parent the ledger never holds).
pub fn header_rows(plan: &BlockPlan, place: &HeaderPlace) -> Result<Vec<String>, String> {
    let mut m = Measured::default();
    header(&Oracle::new(Pin::V24_0), plan, &plan.build(), place, &mut m)?;
    Ok(m.defects.into_iter().map(|(row, _)| row).collect())
}

pub fn measure(plan: &BlockPlan, built: &Built, place: &Place<'_>) -> Result<Measured, String> {
    let oracle = Oracle::new(Pin::V24_0);
    let mut m = Measured::default();
    let h = place.height;
    let dip1 = h >= DIP0001_HEIGHT;
    let raw: Vec<Vec<u8>> = plan.txs.iter().map(|t| core_serialize(&*t.tx())).collect();
    let oracle_err = |what: &str, e: dash_oracle_sys::Error| format!("the oracle's {what}: {e:?}");

    header(&oracle, plan, built, &HeaderPlace::of(place), &mut m)?;

    // CheckBlock (v:4383-4435).
    let (root, mutated) =
        oracle.merkle_root(&built.txids).map_err(|e| oracle_err("BlockMerkleRoot", e))?;
    if built.header[36..68] != root {
        m.add(CMR_ROOT, At::default());
    }
    if mutated {
        m.add(CMR_DUP, At::default());
    }
    let size = block_size(plan);
    let over = |max: usize| plan.txs.is_empty() || plan.txs.len() > max || size > max;
    if over(2_000_000) {
        m.add(CB_LENGTH, At::default());
    }
    for (i, tx) in plan.txs.iter().enumerate().skip(1) {
        if is_coinbase(tx) {
            m.add(CB_MULTIPLE, tx_at(i));
        }
    }
    let mut legacy = 0u64;
    for (i, bytes) in raw.iter().enumerate() {
        if let Verdict::Reject(r) =
            oracle.check_transaction(bytes).map_err(|e| oracle_err("CheckTransaction", e))?
        {
            let row = CHECK_TRANSACTION
                .iter()
                .find(|k| k.rsplit(':').next() == Some(r.reason.as_str()))
                .ok_or_else(|| format!("tx {i}: CheckTransaction's {} is no row", r.reason))?;
            // A loop row's trigger is its input or output index, measured from the bytes; the
            // oracle's reason only confirms the row.
            let at = if LOOP_ROWS.contains(row) {
                match check_transaction_loops(&plan.txs[i], i) {
                    Some((looped, at)) if looped == *row => at,
                    found => {
                        return Err(format!(
                            "tx {i}: CheckTransaction rejects {row}, the bytes' first loop row is \
                             {found:?}"
                        ));
                    }
                }
            } else {
                tx_at(i)
            };
            m.add(row, at);
        }
        legacy +=
            u64::from(oracle.legacy_sigops(bytes).map_err(|e| oracle_err("legacy sigops", e))?);
    }
    if legacy > 40_000 {
        m.add(CB_SIGOPS, At::default());
    }

    // ContextualCheckBlock (v:4542-4605), BIP113's cutoff from CSV (v:4547-4556).
    let cutoff = if h >= CSV_HEIGHT {
        place.mtp
    } else {
        plan.time
    };
    let here = if dip1 {
        2_000_000
    } else {
        1_000_000
    };
    if over(here) {
        m.add(CCB_LENGTH, At::default());
    }
    for (i, tx) in plan.txs.iter().enumerate() {
        if !is_final(tx, h, cutoff) {
            m.add(CCB_FINAL, tx_at(i));
        }
        // v:388 via v:4578: the 100 kB limit once DIP0001 is active.
        if dip1 && tx_size(tx) > 100_000 {
            m.add(CCT_OVERSIZE, tx_at(i));
        }
    }
    if legacy > (here / 50) as u64 {
        m.add(CCB_SIGOPS, At::default());
    }
    // v:4591-4597 (pre-DIP3 on every L3 height).
    let expect = script_num(i64::from(h));
    if plan.txs.first().is_some_and(|cb| !cb.inputs[0].script_sig.starts_with(&expect)) {
        m.add(CCB_HEIGHT, At::default());
    }

    // ConnectBlock (v:2535-2865) over the parent's coins.
    let mut view: BTreeMap<Outpoint, LedgerCoin> = place.coins.coins.clone();
    for (i, txid) in built.txids.iter().enumerate() {
        // v:2615-2621: an output already unspent in the view.
        if (0..plan.txs[i].outputs.len() as u32).any(|n| view.contains_key(&(*txid, n))) {
            m.add(CONN_BIP30, tx_at(i));
        }
    }
    // ProcessSpecialTxsInBlock (v:2674, before the loop): pre-DIP3, a special version with a
    // type is refused (st:224-232), but the type-5 coinbase at 0 (st:925), whose payload a
    // version below 3 never carries (st:884-891).
    for (i, tx) in plan.txs.iter().enumerate() {
        let special = tx.version >= 3;
        let cb5 = i == 0 && tx.tx_type == TRANSACTION_COINBASE;
        if special && tx.tx_type != 0 && !cb5 {
            m.add(STX_DIP3, tx_at(i));
        }
        if cb5 {
            if special {
                return Err("a version-3 type-5 coinbase: its payload is not measured".into());
            }
            m.add(STX_CB_PAYLOAD, At::default());
        }
    }
    let flags = script_flags(h);
    let mut fees = 0i64;
    let mut sigops = 0u64;
    let mut fees_out = false;
    let mut sigops_over = false;
    for (i, (tx, txid)) in plan.txs.iter().zip(&built.txids).enumerate() {
        let coinbase = i == 0 && is_coinbase(tx);
        let spent: Vec<Option<LedgerCoin>> =
            tx.inputs.iter().map(|input| view.get(&input.prevout).cloned()).collect();
        if !coinbase {
            // CheckTxInputs (tv:161-205).
            if spent.iter().any(Option::is_none) {
                m.add(CTI_MISSING, tx_at(i));
            } else {
                let mut value_in = 0i64;
                let mut range = true;
                for (j, coin) in spent.iter().flatten().enumerate() {
                    if coin.3 && h - coin.2 < MATURITY {
                        m.add(CTI_PREMATURE, input_at(i, j));
                    }
                    value_in = value_in.saturating_add(coin.0);
                    if range && (!money_range(coin.0) || !money_range(value_in)) {
                        m.add(CTI_VALUES, input_at(i, j));
                        range = false;
                    }
                }
                let value_out: i64 = tx.outputs.iter().map(|o| o.0).sum();
                if range && value_in < value_out {
                    m.add(CTI_BELOW, tx_at(i));
                }
                if range && value_in >= value_out {
                    fees = fees.saturating_add(value_in - value_out);
                    if !fees_out && !money_range(fees) {
                        m.add(CONN_FEES, tx_at(i));
                        fees_out = true;
                    }
                }
                // v:2742-2744: BIP68 from CSV (nLockTimeFlags, v:2637-2640).
                if h >= CSV_HEIGHT {
                    let heights: Vec<u32> = spent.iter().flatten().map(|c| c.2).collect();
                    match sequence_locks_hold(tx, &heights, h, place.coins) {
                        Some(true) => {}
                        Some(false) => m.add(CONN_BIP68, tx_at(i)),
                        None => return Err(format!("tx {i}: a BIP68 time the ledger lacks")),
                    }
                }
            }
        }
        // v:2748-2754: GetTransactionSigOpCount over the view, cumulative.
        let outs: Vec<SpentOutput<'_>> = spent
            .iter()
            .map(|c| SpentOutput {
                amount: c.as_ref().map_or(0, |c| c.0),
                script_pubkey: c.as_ref().map_or(&[][..], |c| c.1.as_slice()),
            })
            .collect();
        // A transaction spending one prevout twice never gets here in Core (CheckTransaction's
        // bad-txns-inputs-duplicate in CheckBlock's loop, v:4416-4425, returns first), and the
        // oracle refuses its P2SH count: its legacy count stands in.
        let duplicate = {
            let mut seen = BTreeSet::new();
            !tx.inputs.iter().all(|input| seen.insert(input.prevout))
        };
        let count = if coinbase || duplicate || spent.iter().any(Option::is_none) {
            u64::from(oracle.legacy_sigops(&raw[i]).map_err(|e| oracle_err("legacy sigops", e))?)
        } else {
            u64::from(
                oracle
                    .transaction_sigops(&raw[i], &outs, flags)
                    .map_err(|e| oracle_err("GetTransactionSigOpCount", e))?
                    .total,
            )
        };
        sigops += count;
        if !sigops_over && sigops > (here / 50) as u64 {
            m.add(CONN_SIGOPS, tx_at(i));
            sigops_over = true;
        }
        // v:2757-2771: CheckInputScripts, the first failing input.
        if !coinbase && spent.iter().all(Option::is_some) {
            for (j, coin) in spent.iter().flatten().enumerate() {
                let r = oracle
                    .verify_script(&coin.1, &raw[i], j as u32, coin.0, flags)
                    .map_err(|e| oracle_err("VerifyScript", e))?;
                if !r.ok {
                    m.add(CIS_SCRIPT, input_at(i, j));
                    break;
                }
            }
        }
        // UpdateCoins (v:2778): spend the inputs, add the spendable outputs.
        if !coinbase {
            for input in &tx.inputs {
                view.remove(&input.prevout);
            }
        }
        for (n, (value, script)) in tx.outputs.iter().enumerate() {
            if !is_unspendable(script) {
                view.insert((*txid, n as u32), (*value, script.clone(), h, coinbase));
            }
        }
    }
    // v:2857-2861: the coinbase against the subsidy and the fees.
    if let Some(cb) = plan.txs.first().filter(|t| is_coinbase(t)) {
        let paid: i64 = cb.outputs.iter().map(|o| o.0).sum();
        if paid > place.reward.saturating_add(fees) {
            m.add(CONN_AMOUNT, At::default());
        }
    }

    // The setting: iteration facts and domain lengths as the block has them.
    let indices = |f: &dyn Fn(usize, &TxPlan) -> bool| -> BTreeSet<usize> {
        plan.txs.iter().enumerate().filter(|(i, t)| f(*i, t)).map(|(i, _)| i).collect()
    };
    m.facts.iter.insert("coinbase".into(), indices(&|_, t| is_coinbase(t)));
    m.facts.iter.insert("special_version".into(), indices(&|_, t| t.version >= 3));
    m.facts.iter.insert("has_payload".into(), indices(&|_, t| t.version >= 3 && t.tx_type != 0));
    m.facts
        .iter
        .insert("cb_type5_at".into(), indices(&|i, t| i == 0 && t.tx_type == TRANSACTION_COINBASE));
    let cb5 = plan.txs.first().is_some_and(|t| t.tx_type == TRANSACTION_COINBASE);
    m.facts.facts.insert("cb_type5".into(), cb5);
    m.facts.facts.insert("cbtx_decoded".into(), false);
    m.facts.lengths.insert("block.vtx".into(), plan.txs.len());
    m.shapes = plan.txs.iter().map(|t| (t.inputs.len(), t.outputs.len())).collect();
    m.facts
        .lengths
        .insert("tx.vin".into(), plan.txs.iter().map(|t| t.inputs.len()).max().unwrap_or(0));
    m.facts
        .lengths
        .insert("tx.vout".into(), plan.txs.iter().map(|t| t.outputs.len()).max().unwrap_or(0));
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::build::{Input, Lock};
    use crate::chain::mutate::Site;
    use crate::l3::pairs::Dry;

    /// Opus r1 n2: every row `measure` finds is in the model's Submit and Proposal trees (the
    /// Proposal tree keeps CheckBlockHeader's proof of work behind its fCheckPOW guard).
    #[test]
    fn rows_are_the_models() {
        let model = crate::l3::precedence::Model::load().expect("the model");
        let tree = |mode| crate::l3::precedence::rows(&model, mode);
        let (submit, proposal) =
            (tree(crate::chain::mutate::Mode::Submit), tree(crate::chain::mutate::Mode::Proposal));
        let missing = |tree: &BTreeSet<String>| -> Vec<&str> {
            rows().into_iter().filter(|r| !tree.contains(*r)).collect()
        };
        assert_eq!(missing(&submit), Vec::<&str>::new(), "rows the Submit tree lacks");
        assert_eq!(missing(&proposal), Vec::<&str>::new(), "rows the Proposal tree lacks");
    }

    /// S-F3: a CheckTransaction loop row's trigger is its measured input or output index, the
    /// first that fires (tx_check.cpp:39-58, 69-71), never index 0 for want of one; the
    /// oracle's reason names the same row.
    #[test]
    fn check_transaction_loop_rows_carry_their_measured_index() {
        let dry = Dry::new(Site::Reg, &[Lock::P2shTrue; 3], 0);
        let input = |k: usize| {
            let c = dry.coins[k];
            Input::new(c.outpoint, c.lock.script_sig())
        };
        let out = |v: i64| (v, Lock::True.script_pubkey());
        let [.., negative, toolarge, total, duplicate, _, null] = CHECK_TRANSACTION;
        let cases: [(&str, TxPlan, &str, At); 5] = [
            (
                "a duplicate input at j = 2",
                TxPlan::simple(vec![input(0), input(1), input(0)], vec![out(1_000)]),
                duplicate,
                input_at(1, 2),
            ),
            (
                "a negative output at n = 1",
                TxPlan::simple(vec![input(0)], vec![out(5), out(-1), out(7)]),
                negative,
                output_at(1, 1),
            ),
            (
                "an output over MAX_MONEY at n = 2",
                TxPlan::simple(vec![input(0)], vec![out(5), out(7), out(MAX_MONEY + 1)]),
                toolarge,
                output_at(1, 2),
            ),
            (
                "the running total crossing MAX_MONEY first at n = 1",
                TxPlan::simple(vec![input(0)], vec![out(MAX_MONEY), out(1), out(1)]),
                total,
                output_at(1, 1),
            ),
            (
                "a null prevout at j = 1",
                TxPlan::simple(
                    vec![input(0), Input::new(([0; 32], u32::MAX), Vec::new())],
                    vec![out(1_000)],
                ),
                null,
                input_at(1, 1),
            ),
        ];
        for (what, tx, row, at) in cases {
            assert_eq!(check_transaction_loops(&tx, 1), Some((row, at)), "{what}");
            let plan = BlockPlan {
                txs: vec![dry.template.txs[0].clone(), tx],
                ..dry.template.clone()
            };
            let m = measure(&plan, &plan.build(), &dry.place()).expect(what);
            assert!(m.defects.contains(&(row.to_owned(), at)), "{what}: {:?}", m.defects);
            assert!(
                !m.defects.iter().any(|(r, a)| r == row && *a != at),
                "{what}: {:?}",
                m.defects
            );
        }
        // A clean transaction holds no loop row.
        assert_eq!(check_transaction_loops(&TxPlan::simple(vec![input(0)], vec![out(5)]), 1), None);
    }
}
