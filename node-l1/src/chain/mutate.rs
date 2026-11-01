//! The non-evo mutation operators (O2b §3.4, CH3 §7.3's set): each turns the next block the
//! generator builds into one Core must refuse, with the reject strings it expects. A refusal
//! with another string is a generator error (O2b-3 §2.5), so these strings are checked against
//! Core by `l1_chain`'s per-operator test.
//!
//! One catalogue serves both harnesses (CH3a a10, track L plan §2.2): [`OPERATORS`] is
//! `diff_chain`'s, frozen (its order and length are the fuzz corpora's encoding), and
//! [`L3_OPERATORS`] is chained after it by [`catalogue`]. A position in the chained list is never
//! persisted: L3's reports and manifests name an operator by its [`Operator::name`], which is its
//! stable id. Each operator's [`Variant`]s say where L3 runs it and what Core answers there, per
//! mode and `-par`; `diff_chain` builds the first variant and reads only `rejects` and `stage`.

use super::build::{
    BlockPlan, Input, Lock, MAX_FUTURE, OP_RETURN, REGTEST_BITS, TxPlan, coinbase, push,
};
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
    /// L3's trial (`None` under `diff_chain`): what makes each trial's transactions its own.
    pub l3: Option<&'a L3Cx>,
}

/// An L3 trial's part of the context (plan §3.2): its 8-byte id and the variant's carrier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct L3Cx {
    pub trial: [u8; 8],
    pub carrier: Carrier,
}

impl Cx<'_> {
    /// Put the trial's carrier on `tx`, under L3 only: a new trial id gives every transaction an
    /// operator builds a new txid, and so every signature over it a new digest (plan §3.2).
    pub fn carry(&self, tx: &mut TxPlan) {
        let Some(l3) = self.l3 else {
            return;
        };
        match l3.carrier {
            Carrier::OpReturn => tx.outputs.push((0, [vec![OP_RETURN], push(&l3.trial)].concat())),
            // BIP68's disable flag set: the low bits vary the txid and lock nothing.
            Carrier::Sequence => {
                // All eight bytes, folded: two ids differing anywhere differ here too.
                let id = u64::from_le_bytes(l3.trial);
                let id = (id ^ (id >> 32)) as u32;
                tx.inputs[0].sequence = SEQUENCE_DISABLE | (id & !SEQUENCE_DISABLE);
            }
        }
    }
}

/// BIP68's disable flag (`CTxIn::SEQUENCE_LOCKTIME_DISABLE_FLAG`).
pub const SEQUENCE_DISABLE: u32 = 1 << 31;

/// How a variant's transactions carry their trial's id (plan §3.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Carrier {
    /// A zero-value `OP_RETURN` output holding the id, added to each transaction built.
    OpReturn,
    /// The first input's sequence, for variants whose mutation is the output set.
    Sequence,
}

/// How a block reaches the node (plan §2.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Mode {
    /// `submitblock`.
    Submit,
    /// `getblocktemplate` in proposal mode.
    Proposal,
    /// `submitheader`.
    Header,
}

/// The script-check threads (`-par`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Par {
    One,
    Four,
}

impl Par {
    pub fn arg(self) -> &'static str {
        match self {
            Par::One => "-par=1",
            Par::Four => "-par=4",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Par::One => "par1",
            Par::Four => "par4",
        }
    }
}

/// Where on L3's chain a variant runs (plan rev1 §2.2). Only `Reg` has cases in a10-4.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Site {
    /// Regular blocks below the budget heights, every L3 deployment active.
    Reg,
}

impl Site {
    pub fn name(self) -> &'static str {
        match self {
            Site::Reg => "Reg",
        }
    }
}

/// What Core answers a variant's block in one mode at one `-par`: dashd's live reply is the
/// reference, this the harness's self-check (a difference is operator drift).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expect {
    /// Accepted (`null`).
    Accept,
    /// Refused with `reason`, at the census occurrence `occurrence`
    /// (`<file>:<function>#<ordinal>:<template>`, plan §6.1).
    Reject {
        reason: &'static str,
        occurrence: &'static str,
    },
    /// The RPC refuses it before validation.
    Refused {
        code: i64,
        message: &'static str,
    },
}

/// One variant of an operator.
#[derive(Clone, Copy, Debug)]
pub struct Variant {
    pub name: &'static str,
    pub sites: &'static [Site],
    pub expect: fn(Mode, Par) -> Expect,
    pub carrier: Carrier,
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
    /// Its stable id, unique across [`catalogue`].
    pub name: &'static str,
    /// Reject string prefixes, any of which Core may give.
    pub rejects: &'static [&'static str],
    pub stage: Stage,
    /// The operator only means something on S1's check queue (`-par=4`).
    pub par: bool,
    pub apply: fn(&mut BlockPlan, &Cx<'_>),
    /// Where L3 runs it and what Core answers; `diff_chain` builds as the first.
    pub variants: &'static [Variant],
}

const SCRIPT: &str = "mandatory-script-verify-flag-failed";

// The census occurrences the 30 reach (plan §6.2, rc.4).
const CBH_POW: &str = "validation.cpp:CheckBlockHeader#1:high-hash";
const CMR_ROOT: &str = "validation.cpp:CheckMerkleRoot#1:bad-txnmrklroot";
const CMR_DUP: &str = "validation.cpp:CheckMerkleRoot#2:bad-txns-duplicate";
const CB_MISSING: &str = "validation.cpp:CheckBlock#2:bad-cb-missing";
const CB_MULTIPLE: &str = "validation.cpp:CheckBlock#3:bad-cb-multiple";
const CCBH_BITS: &str = "validation.cpp:ContextualCheckBlockHeader#2:bad-diffbits";
const CCBH_OLD: &str = "validation.cpp:ContextualCheckBlockHeader#4:time-too-old";
const CCBH_NEW: &str = "validation.cpp:ContextualCheckBlockHeader#5:time-too-new";
const CCBH_VERSION: &str = "validation.cpp:ContextualCheckBlockHeader#6:bad-version(0x%08x)";
const CCB_FINAL: &str = "validation.cpp:ContextualCheckBlock#2:bad-txns-nonfinal";
const CCB_HEIGHT: &str = "validation.cpp:ContextualCheckBlock#4:bad-cb-height";
const CONN_BIP68: &str = "validation.cpp:ConnectBlock#5:bad-txns-nonfinal";
const CONN_QUEUED: &str = "validation.cpp:ConnectBlock#7:block-validation-failed";
const CONN_AMOUNT: &str = "validation.cpp:ConnectBlock#9:bad-cb-amount";
const CIS_SCRIPT: &str =
    "validation.cpp:CheckInputScripts#2:mandatory-script-verify-flag-failed (%s)";
const CT_NEGATIVE: &str = "tx_check.cpp:CheckTransaction#5:bad-txns-vout-negative";
const CT_TOOLARGE: &str = "tx_check.cpp:CheckTransaction#6:bad-txns-vout-toolarge";
const CT_DUP_INPUT: &str = "tx_check.cpp:CheckTransaction#8:bad-txns-inputs-duplicate";
const CTI_MISSING: &str = "tx_verify.cpp:Consensus::CheckTxInputs#1:bad-txns-inputs-missingorspent";
const CTI_PREMATURE: &str =
    "tx_verify.cpp:Consensus::CheckTxInputs#2:bad-txns-premature-spend-of-coinbase";
const CTI_BELOW: &str = "tx_verify.cpp:Consensus::CheckTxInputs#4:bad-txns-in-belowout";
const STX_DIP3: &str = "specialtxman.cpp:CheckSpecialTxInner#1:bad-tx-type-dip3-inactive";

/// `RPC_DESERIALIZATION_ERROR` (rpc/protocol.h).
const RPC_DESERIALIZATION_ERROR: i64 = -22;

const REG: &[Site] = &[Site::Reg];

/// One `@Reg` variant expecting the same reject in every mode and at every `-par`.
macro_rules! reg {
    ($reason:expr, $occurrence:expr) => {
        reg!($reason, $occurrence, Carrier::OpReturn)
    };
    ($reason:expr, $occurrence:expr, $carrier:expr) => {
        &[Variant {
            name: "reg",
            sites: REG,
            expect: |_, _| Expect::Reject {
                reason: $reason,
                occurrence: $occurrence,
            },
            carrier: $carrier,
        }]
    };
}

/// A script failure: the failing check's own reason inline (`-par=1`), the queue's verdict when
/// the check is queued (`-par=4`, v:2786).
const SCRIPT_VARIANT: &[Variant] = &[Variant {
    name: "reg",
    sites: REG,
    expect: |_, par| match par {
        Par::One => Expect::Reject {
            reason: "mandatory-script-verify-flag-failed",
            occurrence: CIS_SCRIPT,
        },
        Par::Four => Expect::Reject {
            reason: "block-validation-failed",
            occurrence: CONN_QUEUED,
        },
    },
    carrier: Carrier::OpReturn,
}];

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
        variants: reg!("bad-cb-amount", CONN_AMOUNT),
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
        variants: reg!("bad-txns-duplicate", CMR_DUP),
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
        variants: reg!("bad-txnmrklroot", CMR_ROOT),
    },
    Operator {
        name: "bits_minus",
        rejects: &["bad-diffbits"],
        stage: Stage::Header,
        par: false,
        apply: |p, _| p.bits = REGTEST_BITS - 1,
        variants: reg!("bad-diffbits", CCBH_BITS),
    },
    Operator {
        name: "bits_plus",
        rejects: &["high-hash", "bad-diffbits"],
        stage: Stage::Pow,
        par: false,
        apply: |p, _| p.bits = REGTEST_BITS + 1,
        variants: &[Variant {
            name: "reg",
            sites: REG,
            // TestBlockValidity checks no proof of work: the contextual bits check finds it.
            expect: |mode, _| match mode {
                Mode::Proposal => Expect::Reject {
                    reason: "bad-diffbits",
                    occurrence: CCBH_BITS,
                },
                Mode::Submit | Mode::Header => Expect::Reject {
                    reason: "high-hash",
                    occurrence: CBH_POW,
                },
            },
            carrier: Carrier::OpReturn,
        }],
    },
    Operator {
        name: "time_at_mtp",
        rejects: &["time-too-old"],
        stage: Stage::Header,
        par: false,
        apply: |p, cx| p.time = cx.mtp,
        variants: reg!("time-too-old", CCBH_OLD),
    },
    Operator {
        name: "time_too_new",
        rejects: &["time-too-new"],
        stage: Stage::Header,
        par: false,
        apply: |p, cx| p.time = u32::try_from(cx.mocktime + MAX_FUTURE + 1).unwrap_or(u32::MAX),
        variants: reg!("time-too-new", CCBH_NEW),
    },
    Operator {
        name: "version_3",
        rejects: &["bad-version"],
        stage: Stage::Header,
        par: false,
        apply: |p, _| p.version = 3,
        variants: reg!("bad-version(0x00000003)", CCBH_VERSION),
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
        variants: reg!("bad-cb-height", CCB_HEIGHT),
    },
    Operator {
        name: "missing_input",
        rejects: &["bad-txns-inputs-missingorspent"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| {
            let mut tx = TxPlan::simple(
                vec![Input::new(([0xee; 32], 7), Vec::new())],
                vec![(1_000, Lock::True.script_pubkey())],
            );
            cx.carry(&mut tx);
            p.txs.push(tx);
        },
        variants: reg!("bad-txns-inputs-missingorspent", CTI_MISSING),
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
        variants: reg!("bad-txns-inputs-missingorspent", CTI_MISSING),
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
        variants: reg!("bad-txns-inputs-duplicate", CT_DUP_INPUT),
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
            let mut tx = TxPlan::simple(
                vec![Input::new(prevout, Vec::new())],
                vec![(value, Lock::True.script_pubkey())],
            );
            cx.carry(&mut tx);
            p.txs.push(tx);
        },
        variants: reg!("bad-txns-premature-spend-of-coinbase", CTI_PREMATURE),
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
        variants: reg!("bad-txns-in-belowout", CTI_BELOW),
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
        variants: reg!("bad-txns-vout-negative", CT_NEGATIVE, Carrier::Sequence),
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
        variants: reg!("bad-txns-vout-toolarge", CT_TOOLARGE, Carrier::Sequence),
    },
    Operator {
        name: "script_false",
        rejects: &[SCRIPT, "block-validation-failed"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| locked_spend(p, cx, Lock::False),
        variants: SCRIPT_VARIANT,
    },
    Operator {
        name: "sig_not_der",
        rejects: &[SCRIPT, "block-validation-failed"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| locked_spend(p, cx, Lock::NonDer),
        variants: SCRIPT_VARIANT,
    },
    Operator {
        name: "sig_bad",
        rejects: &[SCRIPT, "block-validation-failed"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| locked_spend(p, cx, Lock::BadSig),
        variants: SCRIPT_VARIANT,
    },
    Operator {
        name: "nulldummy",
        rejects: &[SCRIPT, "block-validation-failed"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| locked_spend(p, cx, Lock::NullDummy),
        variants: SCRIPT_VARIANT,
    },
    Operator {
        name: "p2sh_false",
        rejects: &[SCRIPT, "block-validation-failed"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| locked_spend(p, cx, Lock::P2shFalse),
        variants: SCRIPT_VARIANT,
    },
    Operator {
        name: "cltv_unmet",
        rejects: &[SCRIPT, "block-validation-failed"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| locked_spend(p, cx, Lock::Cltv(cx.height + 10)),
        variants: SCRIPT_VARIANT,
    },
    Operator {
        name: "csv_unmet",
        rejects: &[SCRIPT, "block-validation-failed"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| locked_spend(p, cx, Lock::Csv(10)),
        variants: SCRIPT_VARIANT,
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
        variants: reg!("bad-txns-nonfinal", CCB_FINAL),
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
        variants: reg!("bad-txns-nonfinal", CCB_FINAL),
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
        variants: reg!("bad-txns-nonfinal", CONN_BIP68),
    },
    Operator {
        name: "cb_missing",
        // `submitblock` refuses it before validation; a proposal reaches CheckBlock's reason.
        rejects: &["Block does not start with a coinbase", "bad-cb-missing"],
        stage: Stage::Check,
        par: false,
        apply: |p, cx| p.txs[0] = spend(cx, 0, 1),
        variants: &[Variant {
            name: "reg",
            sites: REG,
            // rpc/mining.cpp:1033-1035 refuses it before validation; a proposal reaches CheckBlock.
            expect: |mode, _| match mode {
                Mode::Proposal => Expect::Reject {
                    reason: "bad-cb-missing",
                    occurrence: CB_MISSING,
                },
                Mode::Submit | Mode::Header => Expect::Refused {
                    code: RPC_DESERIALIZATION_ERROR,
                    message: "Block does not start with a coinbase",
                },
            },
            carrier: Carrier::OpReturn,
        }],
    },
    Operator {
        name: "cb_multiple",
        rejects: &["bad-cb-multiple"],
        stage: Stage::Check,
        par: false,
        apply: |p, cx| {
            let mut second = coinbase(cx.height, 1_000, 0xcb2);
            cx.carry(&mut second);
            p.txs.push(second);
        },
        variants: reg!("bad-cb-multiple", CB_MULTIPLE),
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
        variants: reg!("bad-tx-type-dip3-inactive", STX_DIP3),
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
        variants: &[Variant {
            name: "reg",
            sites: REG,
            // CH3 §2.4 row 2: inline, the script's reason first; queued, the loop's BIP68 one.
            expect: |_, par| match par {
                Par::One => Expect::Reject {
                    reason: "mandatory-script-verify-flag-failed",
                    occurrence: CIS_SCRIPT,
                },
                Par::Four => Expect::Reject {
                    reason: "bad-txns-nonfinal",
                    occurrence: CONN_BIP68,
                },
            },
            carrier: Carrier::OpReturn,
        }],
    },
];

/// L3's operators, chained after [`OPERATORS`] by [`catalogue`]: a10-5's families. Never part of
/// `diff_chain`'s encoding.
pub const L3_OPERATORS: &[Operator] = &[Operator {
    // L3's positive control (a10-4 review r1 m-8): an unmutated one-input spend both sides
    // accept, so the rollback, T-seq and the mempool delta run against dashd. `stage` is
    // `diff_chain`'s only and never read here.
    name: "control",
    rejects: &[],
    stage: Stage::Connect,
    par: false,
    apply: |p, cx| p.txs.push(spend(cx, 0, 1)),
    variants: &[Variant {
        name: "reg",
        sites: &[Site::Reg],
        expect: |_, _| Expect::Accept,
        carrier: Carrier::OpReturn,
    }],
}];

/// Every operator: [`OPERATORS`], then [`L3_OPERATORS`].
pub fn catalogue() -> impl Iterator<Item = &'static Operator> {
    OPERATORS.iter().chain(L3_OPERATORS)
}

/// The operator whose id is `name`.
pub fn by_name(name: &str) -> Option<&'static Operator> {
    catalogue().find(|o| o.name == name)
}

/// A transaction spending the context's coin `i` (or a missing one past the end) into `outs`
/// OP_TRUE outputs, with a 1000-satoshi fee.
pub fn spend(cx: &Cx<'_>, i: usize, outs: usize) -> TxPlan {
    let (prevout, value) =
        cx.coins.get(i).map_or((([0xee; 32], i as u32), 100_000), |c| (c.outpoint, c.value));
    let each = (value - 1_000) / outs as i64;
    let mut tx = TxPlan::simple(
        vec![Input::new(prevout, Vec::new())],
        (0..outs).map(|_| (each, Lock::True.script_pubkey())).collect(),
    );
    cx.carry(&mut tx);
    tx
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
    cx.carry(&mut tx);
    p.txs.push(fund);
    p.txs.push(tx);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `OPERATORS` is `diff_chain`'s corpus encoding: its length and order never change (any
    /// growth needs O2b-4's versioned encoding), so L3's grow `L3_OPERATORS` instead.
    #[test]
    fn operators_are_frozen() {
        let names: Vec<&str> = OPERATORS.iter().map(|o| o.name).collect();
        assert_eq!(
            names,
            [
                "cb_amount",
                "duplicate_tx",
                "merkle_root",
                "bits_minus",
                "bits_plus",
                "time_at_mtp",
                "time_too_new",
                "version_3",
                "cb_height",
                "missing_input",
                "double_spend",
                "duplicate_input",
                "premature_coinbase",
                "in_below_out",
                "vout_negative",
                "vout_toolarge",
                "script_false",
                "sig_not_der",
                "sig_bad",
                "nulldummy",
                "p2sh_false",
                "cltv_unmet",
                "csv_unmet",
                "locktime_height",
                "locktime_mtp",
                "bip68_height",
                "cb_missing",
                "cb_multiple",
                "tx_type_dip3",
                "queued_script_then_bip68",
            ]
        );
    }

    #[test]
    fn ids_are_unique_and_every_operator_has_a_variant() {
        let mut names: Vec<&str> = catalogue().map(|o| o.name).collect();
        let n = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), n, "an operator id repeats");
        for op in catalogue() {
            assert!(!op.variants.is_empty(), "{} has no variant", op.name);
            let mut variants: Vec<&str> = op.variants.iter().map(|v| v.name).collect();
            variants.dedup();
            assert_eq!(variants.len(), op.variants.len(), "{}: a variant name repeats", op.name);
        }
    }

    /// Every reject names an occurrence in the census key's form, and its reason is the key's
    /// template (or, for `%08x`, matches it).
    #[test]
    fn rejects_name_their_occurrence() {
        for op in catalogue() {
            for v in op.variants {
                for mode in [Mode::Submit, Mode::Proposal] {
                    for par in [Par::One, Par::Four] {
                        let Expect::Reject {
                            reason,
                            occurrence,
                        } = (v.expect)(mode, par)
                        else {
                            continue;
                        };
                        let (_, template) = occurrence
                            .rsplit_once(".cpp:")
                            .and_then(|(_, rest)| {
                                rest.split_once('#').and_then(|(_, r)| r.split_once(':'))
                            })
                            .unwrap_or_else(|| panic!("{}: bad occurrence {occurrence}", op.name));
                        let prefix = template.split('(').next().expect("a template").trim_end();
                        assert!(
                            reason.starts_with(prefix),
                            "{}: {reason} vs {occurrence}",
                            op.name
                        );
                        assert!(
                            op.rejects.iter().any(|r| reason.starts_with(r)),
                            "{}: {reason} is not one of its rejects",
                            op.name
                        );
                    }
                }
            }
        }
    }

    fn coin(n: u8) -> Coin {
        Coin {
            outpoint: ([n; 32], 0),
            value: 1_000_000,
            lock: Lock::True,
            height: 1,
            coinbase: false,
        }
    }

    fn built(op: &Operator, l3: Option<&L3Cx>) -> BlockPlan {
        let coins = [coin(1), coin(2), coin(3)];
        let cx = Cx {
            height: 300,
            mtp: 1_000,
            mocktime: 2_000,
            coins: &coins,
            immature: Some(coin(9)),
            l3,
        };
        let mut plan = BlockPlan {
            prev: [0; 32],
            version: 0x2000_0000,
            time: 1_500,
            bits: REGTEST_BITS,
            txs: vec![coinbase(300, 5_000, 1)],
            merkle_override: None,
        };
        (op.apply)(&mut plan, &cx);
        plan
    }

    /// Two trials of one operator differ in every transaction it builds, and only there: the
    /// distinct-txid protocol's premise (plan §3.2). Without L3, nothing is added.
    #[test]
    fn carriers_make_each_trial_its_own() {
        for op in OPERATORS {
            let carrier = op.variants[0].carrier;
            let a = L3Cx {
                trial: *b"trial-a\0",
                carrier,
            };
            let b = L3Cx {
                trial: *b"trial-b\0",
                carrier,
            };
            let (plain, pa, pb) = (built(op, None), built(op, Some(&a)), built(op, Some(&b)));
            assert_eq!(pa.txs.len(), plain.txs.len(), "{}", op.name);
            for (i, ((x, y), z)) in pa.txs.iter().zip(&pb.txs).zip(&plain.txs).enumerate() {
                if i == 0 && op.name != "cb_missing" {
                    // The coinbase is the harness's (its twin tag), not the operator's.
                    assert_eq!(x, z, "{}: coinbase", op.name);
                    continue;
                }
                assert_ne!(x.txid(), y.txid(), "{}: tx {i} is the same in two trials", op.name);
                match carrier {
                    Carrier::OpReturn => {
                        assert_eq!(x.outputs.len(), z.outputs.len() + 1, "{}: tx {i}", op.name);
                        assert_eq!(x.outputs[..z.outputs.len()], z.outputs[..], "{}", op.name);
                    }
                    Carrier::Sequence => {
                        assert_eq!(x.outputs, z.outputs, "{}: tx {i}", op.name);
                        assert_ne!(x.inputs[0].sequence & SEQUENCE_DISABLE, 0, "{}", op.name);
                    }
                }
            }
        }
    }
}
