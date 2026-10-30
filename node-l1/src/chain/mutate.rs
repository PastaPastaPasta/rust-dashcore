//! The non-evo mutation operators (O2b §3.4, CH3 §7.3's set): each turns the next block the
//! generator builds into one Core must refuse, with the reject strings it expects. A refusal
//! with another string is a generator error (O2b-3 §2.5), so these strings are checked against
//! Core by `l1_chain`'s per-operator test.

use super::build::{BlockPlan, Input, Lock, MAX_FUTURE, REGTEST_BITS, TxPlan, coinbase};
use super::world::Coin;

/// What an operator may use: the block's height, the median time past and clock, and the
/// coins it may spend (spendable on the parent, unused by the plan, most mature first).
pub struct Cx<'a> {
    pub height: u32,
    pub mtp: u32,
    pub mocktime: i64,
    pub coins: &'a [Coin],
    /// An immature coinbase output on the branch, if any.
    pub immature: Option<Coin>,
}

/// Where Core finds an operator's fault.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// `CheckBlockHeader`'s proof of work: context-free, so found first wherever a block or
    /// header arrives (a proposal skips it, and finds the bits contextually).
    Pow,
    /// The contextual header checks: the block is never indexed, and its header alone is
    /// refused.
    Header,
    /// `CheckBlock`, first on arrival wherever the block lands: Core indexes nothing for it and
    /// never marks the block failed (a header indexed already stays, unmarked).
    Check,
    /// `ContextualCheckBlock` (in `AcceptBlock`), on arrival: the header is kept and marked
    /// failed, the data is not stored.
    Contextual,
    /// `ConnectBlock`: found only when the block is connected (a block Core only stores is
    /// accepted).
    Connect,
}

/// One operator.
pub struct Operator {
    pub name: &'static str,
    /// Reject string prefixes, any of which Core may give.
    pub rejects: &'static [&'static str],
    pub stage: Stage,
    /// The operator only means something on S1's check queue (`-par=4`).
    pub par: bool,
    pub apply: fn(&mut BlockPlan, &Cx<'_>),
}

const SCRIPT: &str = "mandatory-script-verify-flag-failed";

/// The most coins an operator spends (`spend(cx, 2, …)`): a block is mutated only with this
/// many in its context.
pub const COINS: usize = 3;

/// The operators, in their tag order (the intent decoder takes a tag modulo the count, so the
/// order is part of the corpus encoding; append only).
pub const OPERATORS: &[Operator] = &[
    Operator {
        name: "cb_amount",
        rejects: &["bad-cb-amount"],
        stage: Stage::Connect,
        par: false,
        apply: |p, _| p.txs[0].outputs[0].0 += 1,
    },
    Operator {
        name: "duplicate_tx",
        rejects: &["bad-txns-duplicate"],
        stage: Stage::Check,
        par: false,
        apply: |p, cx| {
            // Core's mutation check pairs leaves (2k, 2k + 1): the copies must be one pair.
            if p.txs.len() % 2 == 1 {
                p.txs.push(spend(cx, 1, 1));
            }
            let tx = spend(cx, 0, 2);
            p.txs.push(tx.clone());
            p.txs.push(tx);
        },
    },
    Operator {
        name: "merkle_root",
        rejects: &["bad-txnmrklroot"],
        stage: Stage::Check,
        par: false,
        // Wrong, and unique to the block (its coinbase's): two such blocks on one parent at one
        // time would otherwise share a header.
        apply: |p, _| {
            let mut root = p.txs[0].txid();
            root[0] ^= 0x5a;
            p.merkle_override = Some(root);
        },
    },
    Operator {
        name: "bits_minus",
        rejects: &["bad-diffbits"],
        stage: Stage::Header,
        par: false,
        apply: |p, _| p.bits = REGTEST_BITS - 1,
    },
    Operator {
        name: "bits_plus",
        rejects: &["high-hash", "bad-diffbits"],
        stage: Stage::Pow,
        par: false,
        apply: |p, _| p.bits = REGTEST_BITS + 1,
    },
    Operator {
        name: "time_at_mtp",
        rejects: &["time-too-old"],
        stage: Stage::Header,
        par: false,
        apply: |p, cx| p.time = cx.mtp,
    },
    Operator {
        name: "time_too_new",
        rejects: &["time-too-new"],
        stage: Stage::Header,
        par: false,
        apply: |p, cx| p.time = u32::try_from(cx.mocktime + MAX_FUTURE + 1).unwrap_or(u32::MAX),
    },
    Operator {
        name: "version_3",
        rejects: &["bad-version"],
        stage: Stage::Header,
        par: false,
        apply: |p, _| p.version = 3,
    },
    Operator {
        name: "cb_height",
        rejects: &["bad-cb-height"],
        stage: Stage::Contextual,
        par: false,
        apply: |p, cx| {
            let value = p.txs[0].outputs[0].0;
            p.txs[0] = coinbase(cx.height + 1, value, 0xbad);
        },
    },
    Operator {
        name: "missing_input",
        rejects: &["bad-txns-inputs-missingorspent"],
        stage: Stage::Connect,
        par: false,
        apply: |p, _| {
            p.txs.push(TxPlan::simple(
                vec![Input::new(([0xee; 32], 7), Vec::new())],
                vec![(1_000, Lock::True.script_pubkey())],
            ));
        },
    },
    Operator {
        name: "double_spend",
        rejects: &["bad-txns-inputs-missingorspent"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| {
            p.txs.push(spend(cx, 0, 1));
            let mut again = spend(cx, 0, 1);
            again.outputs[0].0 -= 1;
            p.txs.push(again);
        },
    },
    Operator {
        name: "duplicate_input",
        rejects: &["bad-txns-inputs-duplicate"],
        stage: Stage::Check,
        par: false,
        apply: |p, cx| {
            let mut tx = spend(cx, 0, 1);
            tx.inputs.push(tx.inputs[0].clone());
            p.txs.push(tx);
        },
    },
    Operator {
        name: "premature_coinbase",
        rejects: &["bad-txns-premature-spend-of-coinbase"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| {
            // The parent's coinbase (created at height - 1) is always immature here; without
            // one, the outpoint no output has would fail as a missing input instead.
            let (prevout, value) =
                cx.immature.map_or((([0xee; 32], 0), 1_000), |c| (c.outpoint, c.value - 1_000));
            p.txs.push(TxPlan::simple(
                vec![Input::new(prevout, Vec::new())],
                vec![(value, Lock::True.script_pubkey())],
            ));
        },
    },
    Operator {
        name: "in_below_out",
        rejects: &["bad-txns-in-belowout"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| {
            let mut tx = spend(cx, 0, 1);
            tx.outputs[0].0 = cx.coins[0].value + 1;
            p.txs.push(tx);
        },
    },
    Operator {
        name: "vout_negative",
        rejects: &["bad-txns-vout-negative"],
        stage: Stage::Check,
        par: false,
        apply: |p, cx| {
            let mut tx = spend(cx, 0, 2);
            tx.outputs[1].0 = -1;
            p.txs.push(tx);
        },
    },
    Operator {
        name: "vout_toolarge",
        rejects: &["bad-txns-vout-toolarge"],
        stage: Stage::Check,
        par: false,
        apply: |p, cx| {
            let mut tx = spend(cx, 0, 1);
            tx.outputs[0].0 = 21_000_001 * 100_000_000;
            p.txs.push(tx);
        },
    },
    Operator {
        name: "script_false",
        rejects: &[SCRIPT, "block-validation-failed"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| locked_spend(p, cx, Lock::False),
    },
    Operator {
        name: "sig_not_der",
        rejects: &[SCRIPT, "block-validation-failed"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| locked_spend(p, cx, Lock::NonDer),
    },
    Operator {
        name: "sig_bad",
        rejects: &[SCRIPT, "block-validation-failed"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| locked_spend(p, cx, Lock::BadSig),
    },
    Operator {
        name: "nulldummy",
        rejects: &[SCRIPT, "block-validation-failed"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| locked_spend(p, cx, Lock::NullDummy),
    },
    Operator {
        name: "p2sh_false",
        rejects: &[SCRIPT, "block-validation-failed"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| locked_spend(p, cx, Lock::P2shFalse),
    },
    Operator {
        name: "cltv_unmet",
        rejects: &[SCRIPT, "block-validation-failed"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| locked_spend(p, cx, Lock::Cltv(cx.height + 10)),
    },
    Operator {
        name: "csv_unmet",
        rejects: &[SCRIPT, "block-validation-failed"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| locked_spend(p, cx, Lock::Csv(10)),
    },
    Operator {
        name: "locktime_height",
        rejects: &["bad-txns-nonfinal"],
        stage: Stage::Contextual,
        par: false,
        apply: |p, cx| {
            let mut tx = spend(cx, 0, 1);
            tx.lock_time = cx.height;
            tx.inputs[0].sequence = 0;
            p.txs.push(tx);
        },
    },
    Operator {
        name: "locktime_mtp",
        rejects: &["bad-txns-nonfinal"],
        stage: Stage::Contextual,
        par: false,
        apply: |p, cx| {
            let mut tx = spend(cx, 0, 1);
            tx.lock_time = cx.mtp;
            tx.inputs[0].sequence = 0;
            p.txs.push(tx);
        },
    },
    Operator {
        name: "bip68_height",
        rejects: &["bad-txns-nonfinal"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| {
            let mut tx = spend(cx, 0, 1);
            // Older than any coin the image leaves: a relative lock of 1000 blocks.
            tx.inputs[0].sequence = 1000;
            p.txs.push(tx);
        },
    },
    Operator {
        name: "cb_missing",
        // `submitblock` refuses it before validation; a proposal reaches CheckBlock's reason.
        rejects: &["Block does not start with a coinbase", "bad-cb-missing"],
        stage: Stage::Check,
        par: false,
        apply: |p, cx| p.txs[0] = spend(cx, 0, 1),
    },
    Operator {
        name: "cb_multiple",
        rejects: &["bad-cb-multiple"],
        stage: Stage::Check,
        par: false,
        apply: |p, cx| p.txs.push(coinbase(cx.height, 1_000, 0xcb2)),
    },
    Operator {
        name: "tx_type_dip3",
        rejects: &["bad-tx-type-dip3-inactive"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| {
            let mut tx = spend(cx, 0, 1);
            tx.version = 3;
            tx.tx_type = 1;
            tx.extra_payload = vec![0];
            p.txs.push(tx);
        },
    },
    // CH3 §2.4 row 2 (W-P3a's shape): a script fails, then a later transaction fails BIP68.
    // Under S1 the script check is queued, so Core reports the loop's BIP68 reason; under S0 it
    // runs inline and the script's reason comes first (`l1_chain` pins both).
    Operator {
        name: "queued_script_then_bip68",
        rejects: &["bad-txns-nonfinal", SCRIPT, "block-validation-failed"],
        stage: Stage::Connect,
        par: true,
        apply: |p, cx| {
            locked_spend(p, cx, Lock::BadSig);
            let mut tx = spend(cx, 2, 1);
            tx.inputs[0].sequence = 1000;
            p.txs.push(tx);
        },
    },
];

/// A transaction spending the context's coin `i` (or a missing one past the end) into `outs`
/// OP_TRUE outputs, with a 1000-satoshi fee.
pub fn spend(cx: &Cx<'_>, i: usize, outs: usize) -> TxPlan {
    let (prevout, value) =
        cx.coins.get(i).map_or((([0xee; 32], i as u32), 100_000), |c| (c.outpoint, c.value));
    let each = (value - 1_000) / outs as i64;
    TxPlan::simple(
        vec![Input::new(prevout, Vec::new())],
        (0..outs).map(|_| (each, Lock::True.script_pubkey())).collect(),
    )
}

/// Lock coin 1's value behind `lock` in one transaction and spend it in the next.
fn locked_spend(p: &mut BlockPlan, cx: &Cx<'_>, lock: Lock) {
    let mut fund = spend(cx, 1, 1);
    fund.outputs[0].1 = lock.script_pubkey();
    let fund_value = fund.outputs[0].0;
    let txid = fund.txid();
    let mut tx = TxPlan::simple(
        vec![Input::new((txid, 0), lock.script_sig())],
        vec![(fund_value - 1_000, Lock::True.script_pubkey())],
    );
    if let Lock::Cltv(n) = lock {
        tx.lock_time = n - 11;
        tx.inputs[0].sequence = 0;
    }
    if let Lock::Csv(_) = lock {
        // BIP68 ignores a disabled sequence, which CHECKSEQUENCEVERIFY then refuses (a live
        // relative lock on a coin of the same block would fail BIP68 first).
        tx.inputs[0].sequence = 1 << 31 | 1;
    }
    p.txs.push(fund);
    p.txs.push(tx);
}
