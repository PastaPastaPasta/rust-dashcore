//! The non-evo mutation operators (O2b §3.4, CH3 §7.3's set): each turns the next block the
//! generator builds into one Core must refuse, with the reject strings it expects. A refusal
//! with another string is a generator error (O2b-3 §2.5), so these strings are checked against
//! Core by `l1_chain`'s per-operator test.
//!
//! One catalogue serves both harnesses (CH3a a10, track L plan §2.2): [`OPERATORS`] is
//! `diff_chain`'s, frozen (its order and length are the fuzz corpora's encoding), then
//! [`L3_OPERATORS`] (a10-5's, frozen too) and [`O10_OPERATORS`] are chained after it by
//! [`CATALOGUE`] (O10 plan §2.2). A position in the chained list is never
//! persisted: L3's reports and manifests name an operator by its [`Operator::name`], which is its
//! stable id. Each operator's [`Variant`]s say where L3 runs it and what Core answers there, per
//! mode and `-par`; `diff_chain` builds the first variant and reads only `rejects` and `stage`.

use super::build::{
    BlockPlan, Input, Lock, MAX_FUTURE, OP_0, OP_RETURN, REGTEST_BITS, SIGOPS_MAX, TxPlan,
    coinbase, fill_block, fill_tx, legacy_sigops, push, script_num, tx_size,
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
    pub l3: Option<&'a L3Cx<'a>>,
}

/// An L3 trial's part of the context (plan §3.2): its 8-byte id, the variant and site it runs,
/// and what dashd said of the height.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct L3Cx<'a> {
    pub trial: [u8; 8],
    pub carrier: Carrier,
    pub variant: &'static str,
    pub site: Site,
    /// The block reward at the height: the coinbase value of dashd's coinbase-only template
    /// (its `GetBlockSubsidy`, no fees).
    pub reward: i64,
    /// `getsuperblockbudget` at the height on a superblock site (Core's
    /// `CSuperblock::GetPaymentsLimit`), else 0.
    pub sb_limit: i64,
    /// A coin setup spent before the height (`spent_earlier`'s).
    pub spent: ([u8; 32], u32),
    /// The coinbase at depth 100, mature at the height (`maturity.100`'s).
    pub mature: Coin,
    /// The median time past BIP68 counts coin 0's time locks from (its height's parent's,
    /// `CalculateSequenceLocks`), from dashd.
    pub coin_mtp: u32,
    /// The setup transactions a `REUSE_SETUP` operator repeats: `T_base` (every output
    /// unspent), then `T_base′` (every output spent).
    pub reuse: &'a [TxPlan],
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

/// Where on L3's chain a variant runs (plan rev1 §2.2, §4), on regtest's budget and
/// superblock heights (`nBudgetPaymentsStartBlock` 1000, cycle 50, window 10;
/// `nSuperblockStartBlock` 1500, cycle 20; chainparams.cpp's CRegTestParams).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Site {
    /// Regular blocks below the budget heights, every L3 deployment active.
    Reg,
    /// Before DIP0001 ([`P1_BASE`]..[`DIP0001_HEIGHT`]): 1 MB blocks, 20,000 sigops, no
    /// 100 kB transaction limit.
    P1,
    /// The old budget window (1000-1499, height % 50 < 10).
    BW,
    /// Budget heights off the window (1000-1499, height % 50 ≥ 10).
    BO,
    /// The superblock height `h` (≥ 1500, a multiple of 20).
    SB(u32),
    /// Superblock-era heights that are not superblocks.
    SO,
    /// The height `h`, an activation height or the one below it (group B).
    At(u32),
}

/// DIP0001's height on L3's legs (`-testactivationheight=dip0001@120`) and the first `P1` height:
/// every `P1` case's fan-out (ranks from 0, at `101 + rank`) comes before it.
pub const DIP0001_HEIGHT: u32 = 120;
/// The other activation heights on L3's legs (`-testactivationheight=<name>@<height>`).
pub const DERSIG_HEIGHT: u32 = 130;
pub const CLTV_HEIGHT: u32 = 140;
pub const CSV_HEIGHT: u32 = 150;
pub const BIP147_HEIGHT: u32 = 160;
pub const P1_BASE: u32 = 110;
pub const BUDGET_START: u32 = 1_000;
/// The first `BO` case height: after the `MAX_MONEY` mints of the budget window's first heights
/// (1000-1009, 1050-1059) matured.
pub const BO_BASE: u32 = 1_160;
pub const BUDGET_CYCLE: u32 = 50;
pub const BUDGET_WINDOW: u32 = 10;
pub const SUPERBLOCK_START: u32 = 1_500;
pub const SUPERBLOCK_CYCLE: u32 = 20;

impl Site {
    pub fn name(self) -> String {
        match self {
            Site::Reg => "Reg".into(),
            Site::P1 => "P1".into(),
            Site::BW => "BW".into(),
            Site::BO => "BO".into(),
            Site::SB(h) => format!("SB{h}"),
            Site::SO => "SO".into(),
            Site::At(h) => format!("H{h}"),
        }
    }

    /// Whether tier 1 runs the site (plan rev1 §4's T1 rows); the full set runs every site.
    pub fn tier1(self) -> bool {
        matches!(self, Site::Reg | Site::P1 | Site::At(_))
    }

    /// The first height past the site's window: no height from here holds it (`u32::MAX` for
    /// `SO`, which holds every later non-superblock height, and a pinned height's next).
    pub fn end(self) -> u32 {
        match self {
            Site::Reg => BUDGET_START,
            Site::P1 => DIP0001_HEIGHT,
            Site::BW | Site::BO => SUPERBLOCK_START,
            Site::SB(h) | Site::At(h) => h.saturating_add(1),
            Site::SO => u32::MAX,
        }
    }

    /// Whether the block at `height` is on the site.
    pub fn holds(self, height: u32) -> bool {
        let budget = (BUDGET_START..SUPERBLOCK_START).contains(&height);
        let window = height % BUDGET_CYCLE < BUDGET_WINDOW;
        match self {
            Site::Reg => (DIP0001_HEIGHT..BUDGET_START).contains(&height),
            Site::P1 => (P1_BASE..DIP0001_HEIGHT).contains(&height),
            Site::BW => budget && window,
            Site::BO => budget && !window,
            Site::SB(h) => height == h && Site::superblock(h),
            Site::SO => height >= SUPERBLOCK_START && !Site::superblock(height),
            Site::At(h) => height == h,
        }
    }

    /// Whether `height` is a superblock height (`CSuperblock::IsValidBlockHeight`).
    pub fn superblock(height: u32) -> bool {
        height >= SUPERBLOCK_START && height.is_multiple_of(SUPERBLOCK_CYCLE)
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
    /// The RPC answers this string from the block index before validation, with no occurrence
    /// (`duplicate`, `duplicate-invalid`, `inconclusive-not-best-prevblk`: rpc/mining.cpp:711-720,
    /// :1041-1050).
    Answer(&'static str),
}

/// One variant of an operator.
#[derive(Clone, Copy, Debug)]
pub struct Variant {
    pub name: &'static str,
    pub sites: &'static [Site],
    pub expect: fn(Mode, Par, Site) -> Expect,
    pub carrier: Carrier,
    /// Only the full set runs it; tier 1 runs every other variant at its tier-1 sites.
    pub full_only: bool,
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
// Group X's (a10-5).
const STX_CB_PAYLOAD: &str = "specialtxman.cpp:ProcessSpecialTxsInBlock#2:bad-cbtx-payload";
// Group L's (a10-5).
const CB_LENGTH: &str = "validation.cpp:CheckBlock#1:bad-blk-length";
const CB_SIGOPS: &str = "validation.cpp:CheckBlock#4:bad-blk-sigops";
const CCB_LENGTH: &str = "validation.cpp:ContextualCheckBlock#1:bad-blk-length";
const CCB_SIGOPS: &str = "validation.cpp:ContextualCheckBlock#3:bad-blk-sigops";
const CCT_OVERSIZE: &str = "validation.cpp:ContextualCheckTransaction#5:bad-txns-oversize";
const CONN_SIGOPS: &str = "validation.cpp:ConnectBlock#6:bad-blk-sigops";
const CT_OVERSIZE: &str = "tx_check.cpp:CheckTransaction#3:bad-txns-oversize";
const CT_PAYLOAD: &str = "tx_check.cpp:CheckTransaction#4:bad-txns-payload-oversize";
// Groups S and I's (a10-5).
const CT_VIN_EMPTY: &str = "tx_check.cpp:CheckTransaction#1:bad-txns-vin-empty";
const CT_VOUT_EMPTY: &str = "tx_check.cpp:CheckTransaction#2:bad-txns-vout-empty";
const CT_TXOUTTOTAL: &str = "tx_check.cpp:CheckTransaction#7:bad-txns-txouttotal-toolarge";
const CT_CB_LENGTH: &str = "tx_check.cpp:CheckTransaction#9:bad-cb-length";
const CT_PREVOUT_NULL: &str = "tx_check.cpp:CheckTransaction#10:bad-txns-prevout-null";
const CTI_INPUTVALUES: &str =
    "tx_verify.cpp:Consensus::CheckTxInputs#3:bad-txns-inputvalues-outofrange";
const CONN_BIP30: &str = "validation.cpp:ConnectBlock#2:bad-txns-BIP30";
const CONN_FEES: &str = "validation.cpp:ConnectBlock#4:bad-txns-accumulated-fee-outofrange";

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
            expect: |_, _, _| Expect::Reject {
                reason: $reason,
                occurrence: $occurrence,
            },
            carrier: $carrier,
            full_only: false,
        }]
    };
}

/// One variant expecting the same reject at each of its sites, in every mode and at every `-par`.
/// A named scenario's run at Reg, full set only, expecting its last step's answer.
macro_rules! scenario {
    ($name:expr, $last:expr) => {
        Variant {
            name: $name,
            sites: REG,
            expect: |_, _, _| $last,
            carrier: Carrier::OpReturn,
            full_only: true,
        }
    };
}

macro_rules! reject {
    ($name:expr, $sites:expr, $full_only:expr, $reason:expr, $occurrence:expr) => {
        Variant {
            name: $name,
            sites: $sites,
            expect: |_, _, _| Expect::Reject {
                reason: $reason,
                occurrence: $occurrence,
            },
            carrier: Carrier::OpReturn,
            full_only: $full_only,
        }
    };
}

/// A script failure: the failing check's own reason inline (`-par=1`), the queue's verdict when
/// the check is queued (`-par=4`, v:2791).
const SCRIPT_VARIANT: &[Variant] = &[Variant {
    name: "reg",
    sites: REG,
    expect: |_, par, _| script_reject(par),
    carrier: Carrier::OpReturn,
    full_only: false,
}];

/// A group B variant at the activation height `$height` and the one below: accepted below, and
/// `$at(par)` at it, in every mode.
macro_rules! edge {
    ($name:expr, $height:expr, $at:expr) => {
        Variant {
            name: $name,
            sites: &[Site::At($height - 1), Site::At($height)],
            expect: |_, par, site| {
                if site == Site::At($height) {
                    ($at)(par)
                } else {
                    Expect::Accept
                }
            },
            carrier: Carrier::OpReturn,
            full_only: false,
        }
    };
}

/// A group M family's three orders (plan rev1 §4): the script fault first, F first, or both in
/// one transaction. ConnectBlock's loop stops at the first failing check in transaction order,
/// and a transaction's input, fee, BIP68 and sigop checks run before its scripts; queued
/// (`-par=4`) the scripts wait for the loop's end (v:2791), so F's reason comes first.
macro_rules! orders {
    ($sites:expr, $full_only:expr, $reason:expr, $occurrence:expr) => {
        &[
            Variant {
                name: "script_first",
                sites: $sites,
                expect: |_, par, _| match par {
                    Par::One => script_reject(par),
                    Par::Four => Expect::Reject {
                        reason: $reason,
                        occurrence: $occurrence,
                    },
                },
                carrier: Carrier::OpReturn,
                full_only: $full_only,
            },
            reject!("f_first", $sites, $full_only, $reason, $occurrence),
            reject!("same", $sites, $full_only, $reason, $occurrence),
        ]
    };
}

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
        apply: duplicate_pair,
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
            expect: |mode, _, _| match mode {
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
            full_only: false,
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
            expect: |mode, _, _| match mode {
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
            full_only: false,
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
            expect: |_, par, _| match par {
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
            full_only: false,
        }],
    },
];

/// L3's operators, chained after [`OPERATORS`] by [`catalogue`]: a10-5's families. Never part of
/// `diff_chain`'s encoding.
pub const L3_OPERATORS: &[Operator] = &[
    Operator {
        // L3's positive control (a10-4 review r1 m-8, r2-m1): an unmutated chain both sides accept,
        // so the rollback, T-seq and the mempool delta run against dashd. Its spend of coin 0 and
        // that spend's child are standard (P2SH of `OP_TRUE` from L3's standard funding), so a
        // rollback gives them back to dashd's mempool; the next trial's block spends coin 0 again,
        // conflicting with them (OD-L3-21: `conflict on` coin 0, then `descendant of` the spend).
        // `stage` is `diff_chain`'s only and never read here.
        name: "control",
        rejects: &[],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| {
            let mut spend0 = spend(cx, 0, 1);
            spend0.outputs[0].1 = Lock::P2shTrue.script_pubkey();
            let mut child = TxPlan::simple(
                vec![Input::new((spend0.txid(), 0), Lock::P2shTrue.script_sig())],
                vec![(spend0.outputs[0].0 - 1_000, Lock::P2shTrue.script_pubkey())],
            );
            cx.carry(&mut child);
            p.txs.extend([spend0, child]);
        },
        variants: &[Variant {
            name: "reg",
            sites: &[Site::Reg],
            expect: |_, _, _| Expect::Accept,
            carrier: Carrier::OpReturn,
            full_only: false,
        }],
    },
    // Group V (plan rev1 §4): the coinbase pays the block's bound, its reward plus the fees of
    // every other transaction in the built block (a spend of coin 0), or one satoshi either
    // side; at a superblock, the max, the bound plus `getsuperblockbudget`. The bound is Core's
    // reward from the template plus the fees of the block as built, never the template's
    // value incremented. Core's `IsBlockValueValid` (payments.cpp:262-387, v:2857): over the
    // bound is `bad-cb-amount` but in the old budget window, where ConnectBlock's `NoCheck`
    // skips the check (pay:282-287); with governance unsynced a superblock height checks the
    // max only (pay:347-361). SB1500 is the first superblock, so its rows are the `sb_start`
    // rows too (`bad-sb-start` is unreachable, plan §6.4).
    Operator {
        name: "cb_value",
        rejects: &["bad-cb-amount"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| coinbase_value(p, cx, cx.l3.expect("an L3 family").variant),
        variants: &[
            value_variant("bound-1", V_BOUND),
            value_variant("bound", V_BOUND),
            Variant {
                name: "bound+1",
                sites: V_BOUND,
                expect: |_, _, site| match site {
                    Site::BW => Expect::Accept,
                    _ => CB_OVER,
                },
                carrier: Carrier::OpReturn,
                full_only: false,
            },
            value_variant("max-1", V_MAX),
            value_variant("max", V_MAX),
            Variant {
                name: "max+1",
                sites: V_MAX,
                expect: |_, _, _| CB_OVER,
                carrier: Carrier::OpReturn,
                full_only: false,
            },
        ],
    },
    // Group L (plan rev1 §4, rev5 m-6): the block sigop and size limits and the transaction
    // size limits at and around their bounds (consensus.h: 20,000 sigops and 1 MB before
    // DIP0001, twice that from it; a transaction at most 1 MB, and 100 kB from DIP0001, v:388).
    // Every variant is named by its number: the sigops or bytes it builds.
    Operator {
        name: "sigops_check",
        rejects: &["bad-blk-sigops"],
        stage: Stage::Check,
        par: false,
        apply: |p, cx| legacy_block(p, cx, variant_number(cx)),
        variants: &[
            accept("40000", REG, false),
            reject!("40001", REG, false, "bad-blk-sigops", CB_SIGOPS),
        ],
    },
    Operator {
        // CheckBlock's limit is DIP0001's whatever the height (v:4432): before DIP0001 only
        // the contextual one refuses 20,001.
        name: "sigops_ctx",
        rejects: &["bad-blk-sigops"],
        stage: Stage::Contextual,
        par: false,
        apply: |p, cx| legacy_block(p, cx, variant_number(cx)),
        variants: &[
            accept("20000", P1, false),
            reject!("20001", P1, false, "bad-blk-sigops", CCB_SIGOPS),
        ],
    },
    Operator {
        // The limit in legacy sigops and one P2SH spend: the spend's 13 accurate sigops are
        // the excess the legacy checks never count, so only ConnectBlock's sees it (v:2754).
        name: "sigops_connect",
        rejects: &["bad-blk-sigops"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| {
            let l3 = cx.l3.expect("an L3 family");
            let over = u32::from(l3.variant == "limit+1");
            legacy_block(p, cx, sigop_limit(l3.site) - u32::from(SIGOPS_MAX) + over);
            locked_spend(p, cx, Lock::Sigops(SIGOPS_MAX));
        },
        variants: &[
            accept("limit", P1_REG, false),
            reject!("limit+1", P1_REG, false, "bad-blk-sigops", CONN_SIGOPS),
        ],
    },
    Operator {
        // CheckBlock's size limit is DIP0001's whatever the height (v:4404): before DIP0001
        // the contextual one refuses 1,000,001 bytes.
        name: "block_size",
        rejects: &["bad-blk-length"],
        stage: Stage::Contextual,
        par: false,
        apply: |p, cx| sized_block(p, cx, variant_number(cx) as usize),
        variants: &[
            accept("999999", P1, false),
            accept("1000000", P1, false),
            reject!("1000001", P1, false, "bad-blk-length", CCB_LENGTH),
            accept("1999999", REG, false),
            accept("2000000", REG, false),
            reject!("2000001", REG, false, "bad-blk-length", CB_LENGTH),
        ],
    },
    Operator {
        name: "tx_size_100k",
        rejects: &["bad-txns-oversize"],
        stage: Stage::Contextual,
        par: false,
        apply: |p, cx| sized_tx(p, cx, variant_number(cx) as usize),
        variants: &[
            accept("99999", REG, false),
            accept("100000", REG, false),
            Variant {
                name: "100001",
                sites: P1_REG,
                expect: |_, _, site| match site {
                    Site::P1 => Expect::Accept,
                    _ => Expect::Reject {
                        reason: "bad-txns-oversize",
                        occurrence: CCT_OVERSIZE,
                    },
                },
                carrier: Carrier::OpReturn,
                full_only: false,
            },
        ],
    },
    Operator {
        // rev5 m-6: up to 1 MB the contextual 100 kB check refuses it; over 1 MB, CheckBlock's
        // CheckTransaction does first (tc:33). One spend in a block under 2 MB.
        name: "tx_size_1m",
        rejects: &["bad-txns-oversize"],
        stage: Stage::Contextual,
        par: false,
        apply: |p, cx| sized_tx(p, cx, variant_number(cx) as usize),
        variants: &[
            reject!("999999", REG, true, "bad-txns-oversize", CCT_OVERSIZE),
            reject!("1000000", REG, true, "bad-txns-oversize", CCT_OVERSIZE),
            reject!("1000001", REG, true, "bad-txns-oversize", CT_OVERSIZE),
        ],
    },
    Operator {
        // A ProRegTx-typed spend with an extra payload of the variant's bytes: at the limit it
        // passes CheckTransaction and DIP0003's gate refuses it (st:228); one over, tc:35 does.
        name: "payload_size",
        rejects: &["bad-tx-type-dip3-inactive", "bad-txns-payload-oversize"],
        stage: Stage::Check,
        par: false,
        apply: |p, cx| {
            let mut tx = spend(cx, 0, 1);
            tx.version = 3;
            tx.tx_type = 1;
            tx.extra_payload = vec![0; variant_number(cx) as usize];
            p.txs.push(tx);
        },
        variants: &[
            reject!("10000", REG, true, "bad-tx-type-dip3-inactive", STX_DIP3),
            reject!("10001", REG, true, "bad-txns-payload-oversize", CT_PAYLOAD),
        ],
    },
    // Group S (plan rev1 §4, b10): structure and value ranges, CheckTransaction's in order.
    Operator {
        name: "vin_empty",
        rejects: &["bad-txns-vin-empty"],
        stage: Stage::Check,
        par: false,
        apply: |p, cx| {
            let mut tx = TxPlan::simple(Vec::new(), vec![(0, Lock::True.script_pubkey())]);
            cx.carry(&mut tx);
            p.txs.push(tx);
        },
        variants: &[reject!("reg", REG, false, "bad-txns-vin-empty", CT_VIN_EMPTY)],
    },
    Operator {
        name: "vout_empty",
        rejects: &["bad-txns-vout-empty"],
        stage: Stage::Check,
        par: false,
        apply: |p, cx| {
            let mut tx = spend(cx, 0, 1);
            tx.outputs.clear();
            p.txs.push(tx);
        },
        variants: &[Variant {
            carrier: Carrier::Sequence,
            ..reject!("reg", REG, false, "bad-txns-vout-empty", CT_VOUT_EMPTY)
        }],
    },
    Operator {
        // A second input with the null prevout: one input alone would make it a coinbase.
        name: "prevout_null",
        rejects: &["bad-txns-prevout-null"],
        stage: Stage::Check,
        par: false,
        apply: |p, cx| {
            let mut tx = spend(cx, 0, 1);
            tx.inputs.push(Input::new(([0; 32], u32::MAX), Vec::new()));
            p.txs.push(tx);
        },
        variants: &[reject!("reg", REG, false, "bad-txns-prevout-null", CT_PREVOUT_NULL)],
    },
    Operator {
        // MAX_MONEY across two outputs, or one satoshi more. `max` passes tc:46 everywhere: at
        // BO it spends a minted MAX_MONEY coin and is accepted; at Reg, no coin holding
        // MAX_MONEY, it is the control CheckTxInputs refuses (OD-L3-23 (b), [`LABELS`]).
        name: "txouttotal",
        rejects: &["bad-txns-txouttotal-toolarge", "bad-txns-in-belowout"],
        stage: Stage::Check,
        par: false,
        apply: |p, cx| {
            let over = i64::from(cx.l3.expect("an L3 family").variant == "max+1");
            let mut tx = spend(cx, 0, 1);
            let half = MAX_MONEY / 2;
            tx.outputs = vec![
                (half, Lock::True.script_pubkey()),
                (MAX_MONEY - half + over, Lock::True.script_pubkey()),
            ];
            p.txs.push(tx);
        },
        variants: &[
            Variant {
                name: "max",
                sites: &[Site::Reg, Site::BO],
                expect: |_, _, site| match site {
                    Site::BO => Expect::Accept,
                    _ => Expect::Reject {
                        reason: "bad-txns-in-belowout",
                        occurrence: CTI_BELOW,
                    },
                },
                carrier: Carrier::Sequence,
                full_only: false,
            },
            Variant {
                carrier: Carrier::Sequence,
                ..reject!("max+1", REG, false, "bad-txns-txouttotal-toolarge", CT_TXOUTTOTAL)
            },
        ],
    },
    Operator {
        // The coinbase scriptSig at exactly its variant's length (2 to 100 pass tc:67): the
        // height push, then `OP_0`s. A 2-byte height push needs a height below 128, so `2` runs
        // at P1 (A10-5-NOTES Q-C4-2). The leg adds no tag (EXACT_COINBASE); a carried spend
        // makes each trial its own.
        name: "cb_length",
        rejects: &["bad-cb-length"],
        stage: Stage::Check,
        par: false,
        apply: |p, cx| {
            let n = variant_number(cx) as usize;
            let height = script_num(i64::from(cx.height));
            let script_sig = if n < height.len() {
                vec![OP_0; n]
            } else {
                [height.clone(), vec![OP_0; n - height.len()]].concat()
            };
            p.txs[0].inputs[0].script_sig = script_sig;
            p.txs.push(spend(cx, 0, 1));
        },
        variants: &[
            reject!("1", REG, false, "bad-cb-length", CT_CB_LENGTH),
            accept("2", P1, false),
            accept("100", REG, false),
            reject!("101", REG, false, "bad-cb-length", CT_CB_LENGTH),
        ],
    },
    Operator {
        // One MAX_MONEY input, or two (only mints hold MAX_MONEY: `max_money_coins`).
        name: "inputvalues",
        rejects: &["bad-txns-inputvalues-outofrange"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| {
            let mut tx = spend(cx, 0, 1);
            if cx.l3.expect("an L3 family").variant == "2max" {
                tx.inputs.push(Input::new(cx.coins[1].outpoint, cx.coins[1].lock.script_sig()));
            }
            p.txs.push(tx);
        },
        variants: &[
            accept("max", BO, true),
            reject!("2max", BO, true, "bad-txns-inputvalues-outofrange", CTI_INPUTVALUES),
        ],
    },
    Operator {
        // One transaction paying MAX_MONEY in fees, or two (v:2731's running total).
        name: "accumulated_fee",
        rejects: &["bad-txns-accumulated-fee-outofrange"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| {
            let l3 = cx.l3.expect("an L3 family");
            let n = max_money_coins("accumulated_fee", l3.variant, l3.site);
            for i in 0..n {
                let mut tx = spend(cx, i, 1);
                tx.outputs[0].0 = 0;
                p.txs.push(tx);
            }
        },
        variants: &[
            accept("max", BO, true),
            reject!("2max", BO, true, "bad-txns-accumulated-fee-outofrange", CONN_FEES),
        ],
    },
    // Group I (plan rev1 §4, b8/b9/b11): inputs.
    Operator {
        // The coinbase the split spent at height 101 (a declared absence, plan §3.2).
        name: "spent_earlier",
        rejects: &["bad-txns-inputs-missingorspent"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| {
            let spent = cx.l3.expect("an L3 family").spent;
            let mut tx = TxPlan::simple(
                vec![Input::new(spent, Lock::True.script_sig())],
                vec![(1_000, Lock::True.script_pubkey())],
            );
            cx.carry(&mut tx);
            p.txs.push(tx);
        },
        variants: &[reject!("reg", REG, false, "bad-txns-inputs-missingorspent", CTI_MISSING)],
    },
    Operator {
        // A coinbase at depth 99 (the parent's 98th ancestor's) or 100.
        name: "maturity",
        rejects: &["bad-txns-premature-spend-of-coinbase"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| {
            let l3 = cx.l3.expect("an L3 family");
            let coin = if l3.variant == "99" {
                cx.immature.expect("an immature coin")
            } else {
                l3.mature
            };
            let mut tx = TxPlan::simple(
                vec![Input::new(coin.outpoint, coin.lock.script_sig())],
                vec![(coin.value - 1_000, Lock::True.script_pubkey())],
            );
            cx.carry(&mut tx);
            p.txs.push(tx);
        },
        variants: &[
            reject!("99", REG, false, "bad-txns-premature-spend-of-coinbase", CTI_PREMATURE),
            accept("100", REG, false),
        ],
    },
    Operator {
        // A setup transaction repeated (declared reuse, plan §3.2): `T_base`, every output
        // unspent, overwrites (v:2608-2619); `T_base′`, every output spent, passes BIP30 and
        // fails on its spent input. A carried spend makes each trial its own.
        name: "bip30",
        rejects: &["bad-txns-BIP30", "bad-txns-inputs-missingorspent"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| {
            let l3 = cx.l3.expect("an L3 family");
            p.txs.push(l3.reuse[usize::from(l3.variant == "spent")].clone());
            p.txs.push(spend(cx, 0, 1));
        },
        variants: &[
            reject!("unspent", REG, true, "bad-txns-BIP30", CONN_BIP30),
            reject!("spent", REG, true, "bad-txns-inputs-missingorspent", CTI_MISSING),
        ],
    },
    Operator {
        name: "p2pkh_bad_sig",
        rejects: &[SCRIPT, "block-validation-failed"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| locked_spend(p, cx, Lock::P2pkhBadSig),
        variants: SCRIPT_VARIANT,
    },
    Operator {
        name: "p2sh_bad_sig",
        rejects: &[SCRIPT, "block-validation-failed"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| locked_spend(p, cx, Lock::P2shBadSig),
        variants: SCRIPT_VARIANT,
    },
    // Group B (plan rev1 §4, b2/b6/b13): each rule at the height below its activation (A) and
    // at it (R). Script rules switch with the block's flags (v:2430-2459), BIP68 with
    // ConnectBlock's lock flags (v:2637-2639), BIP113's cutoff and the header versions with the
    // parent's (v:4525-4527, v:4547-4556).
    Operator {
        name: "dersig",
        rejects: &[SCRIPT, "block-validation-failed"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| locked_spend(p, cx, Lock::NonDerNot),
        variants: &[edge!("non_der", DERSIG_HEIGHT, script_reject)],
    },
    Operator {
        // NULLDUMMY is BIP147's (the id `nulldummy` is `diff_chain`'s).
        name: "bip147",
        rejects: &[SCRIPT, "block-validation-failed"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| locked_spend(p, cx, Lock::NullDummy),
        variants: &[edge!("dummy", BIP147_HEIGHT, script_reject)],
    },
    Operator {
        name: "cltv",
        rejects: &[SCRIPT, "block-validation-failed"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| locked_spend(p, cx, Lock::Cltv(cx.height + 10)),
        variants: &[edge!("unmet", CLTV_HEIGHT, script_reject)],
    },
    Operator {
        name: "csv",
        rejects: &[SCRIPT, "block-validation-failed"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| locked_spend(p, cx, Lock::Csv(10)),
        variants: &[edge!("unmet", CSV_HEIGHT, script_reject)],
    },
    Operator {
        // A version-2 spend of a split coin locked 1000 blocks (relative height).
        name: "bip68",
        rejects: &["bad-txns-nonfinal"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| {
            let mut tx = spend(cx, 0, 1);
            tx.inputs[0].sequence = 1000;
            p.txs.push(tx);
        },
        variants: &[edge!("unmet", CSV_HEIGHT, |_| Expect::Reject {
            reason: "bad-txns-nonfinal",
            occurrence: CONN_BIP68,
        })],
    },
    Operator {
        // m-4: a time lock in `[MTP(parent), block time)`, nonfinal sequence with BIP68's
        // disable flag set: final against the block's time, not against the parent's MTP.
        name: "bip113",
        rejects: &["bad-txns-nonfinal"],
        stage: Stage::Contextual,
        par: false,
        apply: |p, cx| {
            let lock_time = p.time - 1;
            assert!(cx.mtp <= lock_time, "bip113: MTP {} over the lock {lock_time}", cx.mtp);
            time_locked(p, cx, lock_time);
        },
        variants: &[edge!("in_interval", CSV_HEIGHT, |_| CB_NONFINAL)],
    },
    Operator {
        // m-4's strict control: the lock at the cutoff itself, the block's time below CSV and
        // the parent's MTP from it, nonfinal on both sides.
        name: "bip113_strict",
        rejects: &["bad-txns-nonfinal"],
        stage: Stage::Contextual,
        par: false,
        apply: |p, cx| {
            assert!(cx.mtp < p.time, "bip113_strict: MTP {} not below the time", cx.mtp);
            let cutoff = if cx.height >= CSV_HEIGHT {
                cx.mtp
            } else {
                p.time
            };
            time_locked(p, cx, cutoff);
        },
        variants: &[Variant {
            name: "cutoff",
            sites: &[Site::At(CSV_HEIGHT - 1), Site::At(CSV_HEIGHT)],
            expect: |_, _, _| CB_NONFINAL,
            carrier: Carrier::OpReturn,
            full_only: false,
        }],
    },
    Operator {
        // A header of version 2 or 3 below and at the activation that outdates it.
        name: "bad_version",
        rejects: &["bad-version"],
        stage: Stage::Header,
        par: false,
        apply: |p, cx| p.version = variant_number(cx) as i32,
        variants: &[
            edge!("2", DERSIG_HEIGHT, |_| Expect::Reject {
                reason: "bad-version(0x00000002)",
                occurrence: CCBH_VERSION,
            }),
            edge!("3", CLTV_HEIGHT, |_| Expect::Reject {
                reason: "bad-version(0x00000003)",
                occurrence: CCBH_VERSION,
            }),
        ],
    },
    // Group X (plan rev1 §4, rev5 m-5; b2, b7, D32, D54): the lock rules at their bounds and
    // the version/type matrix. The ids `locktime_height`, `locktime_mtp` and `bip68_height` are
    // `diff_chain`'s, so the relative-height family is `bip68_blocks`.
    Operator {
        // An absolute lock one below the height or the MTP cutoff (final), at it (not), or far
        // past it with every sequence final (tv:19-36).
        name: "locktime",
        rejects: &["bad-txns-nonfinal"],
        stage: Stage::Contextual,
        par: false,
        apply: |p, cx| {
            let variant = cx.l3.expect("an L3 family").variant;
            let mut tx = spend(cx, 0, 1);
            (tx.lock_time, tx.inputs[0].sequence) = match variant {
                "height-1" => (cx.height - 1, 0),
                "height" => (cx.height, 0),
                "cutoff-1" => (cx.mtp - 1, 0),
                "cutoff" => (cx.mtp, 0),
                "final_seq" => (cx.mtp + 1_000_000, u32::MAX),
                _ => panic!("{variant} is not a locktime variant"),
            };
            p.txs.push(tx);
        },
        variants: &[
            accept("height-1", REG, false),
            reject!("height", REG, false, "bad-txns-nonfinal", CCB_FINAL),
            accept("cutoff-1", REG, false),
            reject!("cutoff", REG, false, "bad-txns-nonfinal", CCB_FINAL),
            accept("final_seq", REG, false),
        ],
    },
    Operator {
        // A relative height lock on coin 0 just met, or short by one block (tv:80-111).
        name: "bip68_blocks",
        rejects: &["bad-txns-nonfinal"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| {
            let short = cx.l3.expect("an L3 family").variant == "short";
            let mut tx = spend(cx, 0, 1);
            tx.inputs[0].sequence = height_lock(cx) - u32::from(!short);
            p.txs.push(tx);
        },
        variants: &[
            accept("met", REG, false),
            reject!("short", REG, false, "bad-txns-nonfinal", CONN_BIP68),
        ],
    },
    Operator {
        // A relative time lock on coin 0 in 512-second units, the most the parent's MTP meets
        // from coin 0's (both dashd's), or one unit more.
        name: "bip68_time",
        rejects: &["bad-txns-nonfinal"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| {
            let l3 = cx.l3.expect("an L3 family");
            let elapsed = cx.mtp - l3.coin_mtp;
            let units = elapsed / 512 + u32::from(l3.variant == "short");
            assert!(units <= SEQUENCE_MASK, "bip68_time: {units} units");
            let mut tx = spend(cx, 0, 1);
            tx.inputs[0].sequence = SEQUENCE_TYPE | units;
            p.txs.push(tx);
        },
        variants: &[
            accept("met", REG, false),
            reject!("short", REG, false, "bad-txns-nonfinal", CONN_BIP68),
        ],
    },
    Operator {
        // A relative height lock short by one block that BIP68 ignores: its disable flag set,
        // or the transaction's version 1 (tv:56, tv:71).
        name: "bip68_off",
        rejects: &[],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| {
            let mut tx = spend(cx, 0, 1);
            tx.inputs[0].sequence = height_lock(cx);
            match cx.l3.expect("an L3 family").variant {
                "disable" => tx.inputs[0].sequence |= SEQUENCE_DISABLE,
                _ => tx.version = 1,
            }
            p.txs.push(tx);
        },
        variants: &[accept("disable", REG, false), accept("v1", REG, false)],
    },
    Operator {
        // rev5 m-5: a type-0 spend at each version with a relative height lock short by one
        // block; BIP68 applies from version 2, the version cast to unsigned (tv:53-57), so -1 is
        // enforced. `-1.disable` sets the disable flag too. Version -1's bytes carry type 0
        // (see `tx_version_type`), a normal transaction.
        name: "tx_version",
        rejects: &["bad-txns-nonfinal"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| {
            let variant = cx.l3.expect("an L3 family").variant;
            let (version, disable) = variant.split_once('.').unwrap_or((variant, ""));
            let mut tx = spend(cx, 0, 1);
            tx.version = version.parse().expect("a version");
            tx.inputs[0].sequence = height_lock(cx);
            if disable == "disable" {
                tx.inputs[0].sequence |= SEQUENCE_DISABLE;
            }
            p.txs.push(tx);
        },
        variants: &[
            reject!("-1", REG, false, "bad-txns-nonfinal", CONN_BIP68),
            accept("0", REG, false),
            accept("1", REG, false),
            reject!("2", REG, false, "bad-txns-nonfinal", CONN_BIP68),
            accept("-1.disable", REG, false),
        ],
    },
    Operator {
        // Type 5 under each version below 3, at index 1: no payload is serialized and the
        // transaction is a normal one. Version -1 fills only the low half of the 32-bit field
        // (`PackVersionAndType`, primitives/transaction.h:237-240 at v24.0.0), so its bytes keep
        // type 5, still normal.
        name: "tx_version_type",
        rejects: &[],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| {
            let variant = cx.l3.expect("an L3 family").variant;
            let mut tx = spend(cx, 0, 1);
            tx.version = variant.parse().expect("a version");
            tx.tx_type = TRANSACTION_COINBASE;
            p.txs.push(tx);
        },
        variants: &[
            accept("-1", REG, false),
            accept("0", REG, false),
            accept("1", REG, false),
            accept("2", REG, false),
        ],
    },
    Operator {
        // Header versions every rule outdates: 0, the sign bit, 1 (v:4525-4527).
        name: "header_version",
        rejects: &["bad-version"],
        stage: Stage::Header,
        par: false,
        apply: |p, cx| {
            let name = cx.l3.expect("an L3 family").variant;
            let bits = u32::from_str_radix(&name[2..], 16).expect("a hex version");
            p.version = bits as i32;
        },
        variants: &[
            reject!("0x00000000", REG, false, "bad-version(0x00000000)", CCBH_VERSION),
            reject!("0x80000000", REG, false, "bad-version(0x80000000)", CCBH_VERSION),
            reject!("0x00000001", REG, false, "bad-version(0x00000001)", CCBH_VERSION),
        ],
    },
    Operator {
        // A version-3 spend of each special type (13 has no validator) at index 1, a
        // one-byte payload: DIP0003's gate refuses it before the payload is read (st:224-229).
        name: "special_tx_type",
        rejects: &["bad-tx-type-dip3-inactive"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| {
            let mut tx = spend(cx, 0, 1);
            tx.version = 3;
            tx.tx_type = variant_number(cx) as u16;
            tx.extra_payload = vec![0];
            p.txs.push(tx);
        },
        variants: &[
            reject!("1", REG, false, "bad-tx-type-dip3-inactive", STX_DIP3),
            reject!("2", REG, false, "bad-tx-type-dip3-inactive", STX_DIP3),
            reject!("3", REG, false, "bad-tx-type-dip3-inactive", STX_DIP3),
            reject!("4", REG, false, "bad-tx-type-dip3-inactive", STX_DIP3),
            reject!("5", REG, false, "bad-tx-type-dip3-inactive", STX_DIP3),
            reject!("6", REG, false, "bad-tx-type-dip3-inactive", STX_DIP3),
            reject!("7", REG, false, "bad-tx-type-dip3-inactive", STX_DIP3),
            reject!("8", REG, false, "bad-tx-type-dip3-inactive", STX_DIP3),
            reject!("9", REG, false, "bad-tx-type-dip3-inactive", STX_DIP3),
            reject!("10", REG, false, "bad-tx-type-dip3-inactive", STX_DIP3),
            reject!("11", REG, false, "bad-tx-type-dip3-inactive", STX_DIP3),
            reject!("12", REG, false, "bad-tx-type-dip3-inactive", STX_DIP3),
            reject!("13", REG, false, "bad-tx-type-dip3-inactive", STX_DIP3),
        ],
    },
    Operator {
        // A version-3 coinbase of type 1 (not 5): step 4 does not skip it, and DIP0003's gate
        // refuses it (st:925-931).
        name: "special_cb_type",
        rejects: &["bad-tx-type-dip3-inactive"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| {
            let cb = &mut p.txs[0];
            (cb.version, cb.tx_type, cb.extra_payload) = (3, 1, vec![0]);
            p.txs.push(spend(cx, 0, 1));
        },
        variants: &[reject!("1", REG, false, "bad-tx-type-dip3-inactive", STX_DIP3)],
    },
    Operator {
        // A type-5 coinbase of version 1 or 2: no payload, so no CbTx decodes (st:876-892).
        name: "cb_type5",
        rejects: &["bad-cbtx-payload"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| {
            let version = variant_number(cx) as i16;
            let cb = &mut p.txs[0];
            (cb.version, cb.tx_type) = (version, TRANSACTION_COINBASE);
            p.txs.push(spend(cx, 0, 1));
        },
        variants: &[
            reject!("1", REG, false, "bad-cbtx-payload", STX_CB_PAYLOAD),
            reject!("2", REG, false, "bad-cbtx-payload", STX_CB_PAYLOAD),
        ],
    },
    // Plan rev5 §4's proposal-only row: no transactions at all, CheckBlock's `vtx.empty()` arm.
    Operator {
        name: "empty_block",
        rejects: &["bad-blk-length"],
        stage: Stage::Check,
        par: false,
        apply: |p, _| p.txs.clear(),
        variants: &[Variant {
            name: "reg",
            sites: REG,
            expect: |mode, _, _| match mode {
                Mode::Proposal => Expect::Reject {
                    reason: "bad-blk-length",
                    occurrence: CB_LENGTH,
                },
                Mode::Submit | Mode::Header => Expect::Refused {
                    code: RPC_DESERIALIZATION_ERROR,
                    message: "Block does not start with a coinbase",
                },
            },
            carrier: Carrier::OpReturn,
            full_only: false,
        }],
    },
    Operator {
        // A version-3 type-0 spend: a normal transaction.
        name: "v3_type0",
        rejects: &[],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| {
            let mut tx = spend(cx, 0, 1);
            tx.version = 3;
            p.txs.push(tx);
        },
        variants: &[accept("reg", REG, false)],
    },
    // Group M (plan rev1 §4, b14, D100): each family F with a script fault (a mandatory-flag
    // failure) in the three orders of [`orders!`]; the Reg families run in tier 1, the BO ones
    // in the full set. Its two excluded rows are [`EXCLUDED`].
    Operator {
        name: "m_missing",
        rejects: &["bad-txns-inputs-missingorspent", SCRIPT],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| {
            matrix(p, cx, |p, cx, extra| {
                let mut tx = TxPlan::simple(
                    vec![Input::new(([0xee; 32], 7), Vec::new())],
                    vec![(1_000, Lock::True.script_pubkey())],
                );
                cx.carry(&mut tx);
                p.txs.push(with_extra(tx, extra));
            })
        },
        variants: orders!(REG, false, "bad-txns-inputs-missingorspent", CTI_MISSING),
    },
    Operator {
        name: "m_premature",
        rejects: &["bad-txns-premature-spend-of-coinbase", SCRIPT],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| {
            matrix(p, cx, |p, cx, extra| {
                let coin = cx.immature.expect("an immature coin");
                let mut tx = TxPlan::simple(
                    vec![Input::new(coin.outpoint, coin.lock.script_sig())],
                    vec![(coin.value - 1_000, Lock::True.script_pubkey())],
                );
                cx.carry(&mut tx);
                p.txs.push(with_extra(tx, extra));
            })
        },
        variants: orders!(REG, false, "bad-txns-premature-spend-of-coinbase", CTI_PREMATURE),
    },
    Operator {
        name: "m_in_belowout",
        rejects: &["bad-txns-in-belowout", SCRIPT],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| {
            matrix(p, cx, |p, cx, extra| {
                let extra_value = extra.as_ref().map_or(0, |e| e.1);
                let mut tx = with_extra(spend(cx, 0, 1), extra);
                tx.outputs[0].0 = cx.coins[0].value + extra_value + 1;
                p.txs.push(tx);
            })
        },
        variants: orders!(REG, false, "bad-txns-in-belowout", CTI_BELOW),
    },
    Operator {
        name: "m_bip68",
        rejects: &["bad-txns-nonfinal", SCRIPT],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| {
            matrix(p, cx, |p, cx, extra| {
                let mut tx = spend(cx, 0, 1);
                tx.inputs[0].sequence = height_lock(cx);
                p.txs.push(with_extra(tx, extra));
            })
        },
        variants: orders!(REG, false, "bad-txns-nonfinal", CONN_BIP68),
    },
    Operator {
        // `sigops_connect.limit+1`'s block: its P2SH spend crosses the limit.
        name: "m_sigops",
        rejects: &["bad-blk-sigops", SCRIPT],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| {
            matrix(p, cx, |p, cx, extra| {
                let site = cx.l3.expect("an L3 family").site;
                legacy_block(p, cx, sigop_limit(site) - u32::from(SIGOPS_MAX) + 1);
                locked_spend(p, cx, Lock::Sigops(SIGOPS_MAX));
                let last = p.txs.pop().expect("the P2SH spend");
                p.txs.push(with_extra(last, extra));
            })
        },
        variants: orders!(REG, false, "bad-blk-sigops", CONN_SIGOPS),
    },
    Operator {
        // Two `MAX_MONEY` inputs (`inputvalues.2max`'s).
        name: "m_inputvalues",
        rejects: &["bad-txns-inputvalues-outofrange", SCRIPT],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| {
            matrix(p, cx, |p, cx, extra| {
                let mut tx = spend(cx, 0, 1);
                tx.inputs.push(Input::new(cx.coins[1].outpoint, cx.coins[1].lock.script_sig()));
                // The fault's input stays unpaid: the output is MAX_MONEY less the fee already,
                // and more fails CheckTransaction first (CheckTransaction#6, vout-toolarge).
                tx.inputs.extend(extra.map(|(input, _)| input));
                p.txs.push(tx);
            })
        },
        variants: orders!(BO, true, "bad-txns-inputvalues-outofrange", CTI_INPUTVALUES),
    },
    Operator {
        // Two transactions paying `MAX_MONEY` in fees (`accumulated_fee.2max`'s); the fault's
        // transactions pay none. In `same` the second fee is the fault's input alone, paid in
        // full: beside a `MAX_MONEY` input it would leave the input range first
        // (CheckTxInputs#3, f1-full 10-31).
        name: "m_accumulated_fee",
        rejects: &["bad-txns-accumulated-fee-outofrange", SCRIPT],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| {
            matrix(p, cx, |p, cx, extra| {
                let mut first = spend(cx, 0, 1);
                first.outputs[0].0 = 0;
                p.txs.push(first);
                let mut second = match extra {
                    Some((input, _)) => {
                        let mut tx =
                            TxPlan::simple(vec![input], vec![(0, Lock::True.script_pubkey())]);
                        cx.carry(&mut tx);
                        tx
                    }
                    None => spend(cx, 1, 1),
                };
                second.outputs[0].0 = 0;
                p.txs.push(second);
            })
        },
        variants: orders!(BO, true, "bad-txns-accumulated-fee-outofrange", CONN_FEES),
    },
    Operator {
        // r7: the fault and a coinbase one satoshi over its reward (the fault's transactions
        // pay no fee): the script verdict comes first (v:2791 before v:2857).
        name: "m_script_cb_amount",
        rejects: &[SCRIPT, "block-validation-failed"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| {
            p.txs.extend(fault(cx));
            p.txs[0].outputs[0].0 += 1;
        },
        variants: SCRIPT_VARIANT,
    },
    Operator {
        // r7: the fault and a version-3 type-1 spend, in either order: ProcessSpecialTxsInBlock
        // refuses the spend before the loop reaches a script (v:2674).
        name: "m_script_dip3_gate",
        rejects: &["bad-tx-type-dip3-inactive"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| {
            let mut special = spend(cx, 0, 1);
            (special.version, special.tx_type, special.extra_payload) = (3, 1, vec![0]);
            let fault = fault(cx);
            if cx.l3.expect("an L3 family").variant == "script_first" {
                p.txs.extend(fault);
                p.txs.push(special);
            } else {
                p.txs.push(special);
                p.txs.extend(fault);
            }
        },
        variants: &[
            reject!("script_first", REG, false, "bad-tx-type-dip3-inactive", STX_DIP3),
            reject!("gate_first", REG, false, "bad-tx-type-dip3-inactive", STX_DIP3),
        ],
    },
    Operator {
        // r7: the fault under a version-1 type-5 coinbase: its payload check runs before the
        // loop (v:2674, st:876-892).
        name: "m_script_cbtx_payload",
        rejects: &["bad-cbtx-payload"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| {
            (p.txs[0].version, p.txs[0].tx_type) = (1, TRANSACTION_COINBASE);
            p.txs.extend(fault(cx));
        },
        variants: &[reject!("reg", REG, false, "bad-cbtx-payload", STX_CB_PAYLOAD)],
    },
    // Plan §4 group P: the named scenarios, full set only. Their steps are
    // [`scenario_steps`]'; `apply` builds the first step's block on the tip, and the leg the
    // rest from it. A variant's `expect` is its last step's.
    Operator {
        name: "s1_mutated_then_honest",
        rejects: &["bad-txns-duplicate"],
        stage: Stage::Check,
        par: false,
        apply: duplicate_pair,
        variants: &[scenario!("reg", Expect::Accept)],
    },
    Operator {
        name: "s2_child_of_invalid",
        rejects: &["bad-cb-amount", "bad-prevblk"],
        stage: Stage::Connect,
        par: false,
        apply: failed_first,
        variants: &[scenario!("reg", PREV_FAILED)],
    },
    Operator {
        name: "s3_resubmit",
        rejects: &["bad-cb-amount"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| {
            if cx.l3.expect("an L3 family").variant == "failed" {
                failed_first(p, cx);
            }
        },
        variants: &[
            scenario!("failed", Expect::Answer("duplicate-invalid")),
            scenario!("valid", Expect::Answer("duplicate")),
        ],
    },
    Operator {
        name: "s3h_failed_header_resubmit",
        rejects: &["bad-cb-amount", "duplicate"],
        stage: Stage::Connect,
        par: false,
        apply: failed_first,
        variants: &[scenario!("reg", HEADER_DUP)],
    },
    Operator {
        name: "s4_proposal_off_tip",
        rejects: &[],
        stage: Stage::Check,
        par: false,
        apply: |_, _| {},
        variants: &[scenario!("reg", Expect::Answer("inconclusive-not-best-prevblk"))],
    },
    Operator {
        name: "s5_unknown_parent",
        rejects: &["prev-blk-not-found"],
        stage: Stage::Header,
        par: false,
        apply: |_, _| {},
        variants: &[scenario!("reg", MISSING_PREV)],
    },
    Operator {
        name: "s6_child_of_unmarked_descendant",
        rejects: &["bad-cb-amount", "bad-prevblk"],
        stage: Stage::Connect,
        par: false,
        apply: failed_first,
        variants: &[scenario!("header", PREV_WALK), scenario!("submit", PREV_WALK)],
    },
];

/// A named scenario's step (plan §4 group P): its name, how its block reaches the node, and
/// what Core answers.
pub type ScenarioStep = (&'static str, Mode, Expect);

/// A named scenario run's steps, by operator and variant; `None` for an operator that is no
/// scenario.
pub fn scenario_steps(op: &str, variant: &str) -> Option<&'static [ScenarioStep]> {
    use Mode::{Header, Proposal, Submit};
    const DUP: Expect = Expect::Reject {
        reason: "bad-txns-duplicate",
        occurrence: CMR_DUP,
    };
    Some(match (op, variant) {
        // v:4832/v:2093: a MUTATED block is not cached invalid, so its honest form is new.
        ("s1_mutated_then_honest", "reg") => {
            &[("mutated", Submit, DUP), ("honest", Submit, Expect::Accept)]
        }
        ("s2_child_of_invalid", "reg") => {
            &[("parent", Submit, CB_OVER), ("child", Submit, PREV_FAILED)]
        }
        ("s3_resubmit", "failed") => {
            &[("first", Submit, CB_OVER), ("again", Submit, Expect::Answer("duplicate-invalid"))]
        }
        ("s3_resubmit", "valid") => {
            &[("first", Submit, Expect::Accept), ("again", Submit, Expect::Answer("duplicate"))]
        }
        ("s3h_failed_header_resubmit", "reg") => {
            &[("block", Submit, CB_OVER), ("header", Header, HEADER_DUP)]
        }
        ("s4_proposal_off_tip", "reg") => {
            &[("prop", Proposal, Expect::Answer("inconclusive-not-best-prevblk"))]
        }
        ("s5_unknown_parent", "reg") => &[("submit", Submit, MISSING_PREV)],
        // F′ and C′ headers-only, F′'s block fails ConnectBlock (C′ stays unmarked), then D′ on
        // C′ (plan rev5 §4).
        ("s6_child_of_unmarked_descendant", "header") => &[
            ("f_header", Header, Expect::Accept),
            ("c_header", Header, Expect::Accept),
            ("f_block", Submit, CB_OVER),
            ("d_header", Header, PREV_WALK),
        ],
        ("s6_child_of_unmarked_descendant", "submit") => &[
            ("f_header", Header, Expect::Accept),
            ("c_header", Header, Expect::Accept),
            ("f_block", Submit, CB_OVER),
            ("d_block", Submit, PREV_WALK),
        ],
        _ => return None,
    })
}

/// `duplicate_tx`'s mutation (CVE-2012-2459's form): a spend of coin 0 twice at the end, as
/// one pair of leaves (Core's mutation check pairs leaves 2k, 2k + 1).
fn duplicate_pair(p: &mut BlockPlan, cx: &Cx<'_>) {
    if p.txs.len() % 2 == 1 {
        p.txs.push(spend(cx, 1, 1));
    }
    let tx = spend(cx, 0, 2);
    p.txs.push(tx.clone());
    p.txs.push(tx);
}

/// Group V's coinbase value: a spend of coin 0, and the coinbase paying the reward and fees
/// with `variant`'s offset.
fn coinbase_value(p: &mut BlockPlan, cx: &Cx<'_>, variant: &str) {
    let l3 = cx.l3.expect("an L3 family");
    p.txs.push(spend(cx, 0, 1));
    let target = l3.reward + fees(p, cx.coins) + value_offset(variant, l3.sb_limit);
    let others: i64 = p.txs[0].outputs[1..].iter().map(|o| o.0).sum();
    p.txs[0].outputs[0].0 = target - others;
}

/// A scenario's failed block F: `cb_value.bound+1`'s, refused by ConnectBlock at Reg and
/// marked BLOCK_FAILED_VALID.
fn failed_first(p: &mut BlockPlan, cx: &Cx<'_>) {
    coinbase_value(p, cx, "bound+1");
}

/// Group M's excluded rows (plan rev1 §4): in every manifest with status `excluded`, never run.
pub const EXCLUDED: &[(&str, &str)] = &[
    ("m_conflict_tx_lock", "conflict-tx-lock: InstantSend locks (M4)"),
    ("m_bad_cb_payee", "bad-cb-payee: D49 regime, mnsync not synced"),
];

/// Group M's script fault, both transactions fee-free: the mature coinbase moved behind
/// `Lock::False`, and its spend, whose script ends false (`SCRIPT_ERR_EVAL_FALSE`, a mandatory
/// failure). Also the failing input and its value, for a transaction that holds both faults.
fn fault_parts(cx: &Cx<'_>) -> (TxPlan, TxPlan, (Input, i64)) {
    let coin = cx.l3.expect("an L3 family").mature;
    let mut fund = TxPlan::simple(
        vec![Input::new(coin.outpoint, coin.lock.script_sig())],
        vec![(coin.value, Lock::False.script_pubkey())],
    );
    cx.carry(&mut fund);
    let input = Input::new((fund.txid(), 0), Lock::False.script_sig());
    let mut spend =
        TxPlan::simple(vec![input.clone()], vec![(coin.value, Lock::True.script_pubkey())]);
    cx.carry(&mut spend);
    (fund, spend, (input, coin.value))
}

/// The script fault's two transactions.
fn fault(cx: &Cx<'_>) -> [TxPlan; 2] {
    let (fund, spend, _) = fault_parts(cx);
    [fund, spend]
}

/// Group M's block: F's transactions (`f` pushes them, the failing one last, holding the extra
/// input it is given) and the script fault, in the variant's order.
fn matrix(p: &mut BlockPlan, cx: &Cx<'_>, f: fn(&mut BlockPlan, &Cx<'_>, Option<(Input, i64)>)) {
    let (fund, spend, failing) = fault_parts(cx);
    match cx.l3.expect("an L3 family").variant {
        "script_first" => {
            p.txs.extend([fund, spend]);
            f(p, cx, None);
        }
        "f_first" => {
            f(p, cx, None);
            p.txs.extend([fund, spend]);
        }
        "same" => {
            p.txs.push(fund);
            f(p, cx, Some(failing));
        }
        other => panic!("{other} is not a group M order"),
    }
}

/// `tx` with the extra input, if any, its value paid to the first output (the fee unchanged).
fn with_extra(mut tx: TxPlan, extra: Option<(Input, i64)>) -> TxPlan {
    if let Some((input, value)) = extra {
        tx.inputs.push(input);
        tx.outputs[0].0 += value;
    }
    tx
}

/// `TRANSACTION_COINBASE`.
const TRANSACTION_COINBASE: u16 = 5;
/// BIP68's type flag and value mask (`CTxIn::SEQUENCE_LOCKTIME_TYPE_FLAG`, `_MASK`).
const SEQUENCE_TYPE: u32 = 1 << 22;
const SEQUENCE_MASK: u32 = 0x0000_ffff;

/// A relative height lock on coin 0 short by exactly one block at the context's height: its
/// `nMinHeight` (coin height + lock - 1) is the height itself (tv:96, tv:107).
fn height_lock(cx: &Cx<'_>) -> u32 {
    let lock = cx.height - cx.coins[0].height + 1;
    assert!(lock <= SEQUENCE_MASK, "a {lock}-block relative lock");
    lock
}

const CB_NONFINAL: Expect = Expect::Reject {
    reason: "bad-txns-nonfinal",
    occurrence: CCB_FINAL,
};

/// A spend of coin 0 time-locked at `lock_time` (a time, over `LOCKTIME_THRESHOLD`), its
/// sequence nonfinal with BIP68's disable flag set.
fn time_locked(p: &mut BlockPlan, cx: &Cx<'_>, lock_time: u32) {
    let mut tx = spend(cx, 0, 1);
    tx.lock_time = lock_time;
    tx.inputs[0].sequence = u32::MAX - 1;
    p.txs.push(tx);
}

/// A script failure's reject at `-par` (see [`SCRIPT_VARIANT`]).
fn script_reject(par: Par) -> Expect {
    match par {
        Par::One => Expect::Reject {
            reason: "mandatory-script-verify-flag-failed",
            occurrence: CIS_SCRIPT,
        },
        Par::Four => Expect::Reject {
            reason: "block-validation-failed",
            occurrence: CONN_QUEUED,
        },
    }
}

const BO: &[Site] = &[Site::BO];

/// `MAX_MONEY` (amount.h).
pub const MAX_MONEY: i64 = 21_000_000 * 100_000_000;

/// The operators whose coinbase scriptSig is the mutation: the leg adds no trial tag to it.
pub const EXACT_COINBASE: &[&str] = &["cb_length"];

/// The operators that also run in Header mode, through `submitheader` (plan rev5 §4).
pub const HEADER_MODE: &[&str] = &["bits_plus", "time_too_new", "time_at_mtp", "version_3"];

/// The operators that run as proposals only (plan rev5 §4: `submitblock` refuses a block with
/// no coinbase before validation, mi:1033).
pub const PROPOSAL_ONLY: &[&str] = &["empty_block"];

/// The operators that repeat a setup transaction (declared reuse, plan §3.2).
pub const REUSE_SETUP: &[&str] = &["bip30"];

/// The report's label of a case whose expectation is not what its name says: (operator,
/// variant, site, label).
pub const LABELS: &[(&str, &str, Site, &str)] =
    &[("txouttotal", "max", Site::Reg, "control: tc:46 passed")];

/// A case's [`LABELS`] entry.
pub fn label(op: &str, variant: &str, site: Site) -> Option<&'static str> {
    LABELS.iter().find(|l| (l.0, l.1, l.2) == (op, variant, site)).map(|l| l.3)
}

/// How many `MAX_MONEY` coins (the leg's mints) a variant spends at a site, as its coins.
pub fn max_money_coins(op: &str, variant: &str, site: Site) -> usize {
    match (op, variant) {
        ("txouttotal", "max") if site == Site::BO => 1,
        ("inputvalues" | "accumulated_fee", "max") => 1,
        ("inputvalues" | "accumulated_fee", "2max") => 2,
        ("m_inputvalues" | "m_accumulated_fee", _) => 2,
        _ => 0,
    }
}

const P1: &[Site] = &[Site::P1];
const P1_REG: &[Site] = &[Site::P1, Site::Reg];

/// The bytes the leg adds to an L3 block's coinbase after `apply`: the trial's 8-byte id, one
/// push (the scriptSig stays under 253 bytes, so its compact size does not grow).
pub const COINBASE_TAG: usize = 9;

/// The number a group L variant is named by.
fn variant_number(cx: &Cx<'_>) -> u32 {
    let name = cx.l3.expect("an L3 family").variant;
    name.parse().unwrap_or_else(|_| panic!("{name} is not a number"))
}

/// `MaxBlockSigOps` at the site (consensus.h).
fn sigop_limit(site: Site) -> u32 {
    if site == Site::P1 {
        20_000
    } else {
        40_000
    }
}

/// `n` legacy sigops in the block: a spend of coin 0 with zero-value `OP_CHECKSIG` outputs
/// (`MAX_SCRIPT_SIZE` each at most). Nothing else an L3 block holds counts one.
fn legacy_block(p: &mut BlockPlan, cx: &Cx<'_>, n: u32) {
    let mut tx = spend(cx, 0, 1);
    let mut left = n as usize;
    while left > 0 {
        let k = left.min(10_000);
        tx.outputs.push((0, legacy_sigops(k)));
        left -= k;
    }
    p.txs.push(tx);
}

/// A spend of coin 0 of exactly `size` bytes.
fn sized_tx(p: &mut BlockPlan, cx: &Cx<'_>, size: usize) {
    p.txs.push(fill_tx(&spend(cx, 0, 1), size).expect("a transaction filled to the byte"));
}

/// The block grown to exactly `size` bytes as the leg serializes it (its coinbase tag
/// included): a chain of spends from coin 0, each under the site's transaction limit (1 MB
/// before DIP0001, 100 kB from it), the last filled to the byte.
fn sized_block(p: &mut BlockPlan, cx: &Cx<'_>, size: usize) {
    let l3 = cx.l3.expect("an L3 family");
    let cap = if l3.site == Site::P1 {
        1_000_000
    } else {
        100_000
    };
    let target = size - COINBASE_TAG;
    let mut tx = spend(cx, 0, 1);
    while target - block_size(p) > cap {
        let full = fill_tx(&tx, cap - 1_000).expect("a filled link");
        let (prevout, value) = ((full.txid(), 0), full.outputs[0].0);
        p.txs.push(full);
        tx = TxPlan::simple(
            vec![Input::new(prevout, Lock::True.script_sig())],
            vec![(value - 1_000, Lock::True.script_pubkey())],
        );
        cx.carry(&mut tx);
    }
    p.txs.push(tx);
    *p = fill_block(p, p.txs.len() - 1, target).expect("a block filled to the byte");
}

/// A block's serialized size.
pub fn block_size(p: &BlockPlan) -> usize {
    80 + super::build::compact_size_len(p.txs.len()) + p.txs.iter().map(tx_size).sum::<usize>()
}

/// Group V's bound sites and superblock sites.
const V_BOUND: &[Site] = &[Site::Reg, Site::BW, Site::BO, Site::SO];
const V_MAX: &[Site] = &[Site::SB(1_500), Site::SB(1_520)];

const CB_OVER: Expect = Expect::Reject {
    reason: "bad-cb-amount",
    occurrence: CONN_AMOUNT,
};

// Group P's AcceptBlockHeader occurrences (rc.4 v:4620, :4639, :4646, :4694).
const HEADER_DUP: Expect = Expect::Reject {
    reason: "duplicate",
    occurrence: "validation.cpp:AcceptBlockHeader#1:duplicate",
};
const MISSING_PREV: Expect = Expect::Reject {
    reason: "prev-blk-not-found",
    occurrence: "validation.cpp:AcceptBlockHeader#3:prev-blk-not-found",
};
const PREV_FAILED: Expect = Expect::Reject {
    reason: "bad-prevblk",
    occurrence: "validation.cpp:AcceptBlockHeader#4:bad-prevblk",
};
const PREV_WALK: Expect = Expect::Reject {
    reason: "bad-prevblk",
    occurrence: "validation.cpp:AcceptBlockHeader#6:bad-prevblk",
};

/// A group V variant every site accepts.
const fn value_variant(name: &'static str, sites: &'static [Site]) -> Variant {
    accept(name, sites, false)
}

/// A variant every site accepts.
const fn accept(name: &'static str, sites: &'static [Site], full_only: bool) -> Variant {
    Variant {
        name,
        sites,
        expect: |_, _, _| Expect::Accept,
        carrier: Carrier::OpReturn,
        full_only,
    }
}

/// What a group V variant adds to the bound: the superblock limit for a `max` row, then its
/// satoshi.
pub fn value_offset(variant: &str, sb_limit: i64) -> i64 {
    match variant {
        "bound-1" => -1,
        "bound" => 0,
        "bound+1" => 1,
        "max-1" => sb_limit - 1,
        "max" => sb_limit,
        "max+1" => sb_limit + 1,
        _ => panic!("{variant} is not a group V variant"),
    }
}

/// The fees of a block's non-coinbase transactions: what their inputs spend (the context's
/// coins, or outputs of earlier transactions in the block) less what they pay.
pub fn fees(p: &BlockPlan, coins: &[Coin]) -> i64 {
    let mut values: std::collections::HashMap<([u8; 32], u32), i64> =
        coins.iter().map(|c| (c.outpoint, c.value)).collect();
    let mut fees = 0;
    for tx in &p.txs {
        if tx.inputs.first().is_some_and(|i| i.prevout == ([0; 32], u32::MAX)) {
            continue;
        }
        let spent: i64 = tx
            .inputs
            .iter()
            .map(|i| *values.get(&i.prevout).expect("an input the block or its coins hold"))
            .sum();
        fees += spent - tx.outputs.iter().map(|o| o.0).sum::<i64>();
        let txid = tx.txid();
        for (n, out) in tx.outputs.iter().enumerate() {
            values.insert((txid, n as u32), out.0);
        }
    }
    fees
}

/// The AV fixtures' defective blocks Dᵢ (plan §7.4, M-10): each valid but for one defect that
/// `-assumevalid` lets through. Not in the catalogue: only the AV fixtures build them.
pub const AV_OPERATORS: &[Operator] = &[
    Operator {
        // D1: a mandatory script failure in one transaction, as `script_false`.
        name: "av_script",
        rejects: &[SCRIPT],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| locked_spend(p, cx, Lock::False),
        variants: SCRIPT_VARIANT,
    },
    Operator {
        // D2/D3: the template's coinbase alone, raw type 5 of version 1 or 2 (no CbTx decodes,
        // st:876-892). Coinbase-only, so the AV-on body's E-evo delta is the plan's (§3.6 m-8).
        name: "av_cb_type5",
        rejects: &["bad-cbtx-payload"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| {
            let version = variant_number(cx) as i16;
            (p.txs[0].version, p.txs[0].tx_type) = (version, TRANSACTION_COINBASE);
        },
        variants: &[
            reject!("1", REG, true, "bad-cbtx-payload", STX_CB_PAYLOAD),
            reject!("2", REG, true, "bad-cbtx-payload", STX_CB_PAYLOAD),
        ],
    },
];

/// The OD-L3-18 MNHF witness's operator (plan §9), outside the catalogue like
/// [`AV_OPERATORS`]: the coinbase, then Core-mined MNHF signal transactions ([`mnhf_signals`])
/// in a pre-DIP3 body, which DIP3's gate refuses before step 11 (st:224-229). Variant `one`
/// carries the first recorded signal; `dup` carries it twice, whose extraction fails
/// (`bad-mnhf-duplicates-in-block`, mh:190-208) though the body is refused first all the same.
/// The copy is the odd last leaf, so the merkle root is not a mutated one (CVE-2012-2459).
pub const MNHF_OPERATORS: &[Operator] = &[Operator {
    name: "mnhf_signal",
    rejects: &["bad-tx-type-dip3-inactive"],
    stage: Stage::Connect,
    par: false,
    apply: |p, cx| {
        let first = mnhf_signals().first().expect("a recorded signal");
        let signal = TxPlan::decode(&first.hex).expect("a recorded signal decodes");
        let copies = if cx.l3.expect("an L3 family").variant == "dup" {
            2
        } else {
            1
        };
        p.txs.truncate(1);
        p.txs.extend(std::iter::repeat_n(signal, copies));
    },
    variants: &[
        reject!("one", REG, true, "bad-tx-type-dip3-inactive", STX_DIP3),
        reject!("dup", REG, true, "bad-tx-type-dip3-inactive", STX_DIP3),
    ],
}];

/// A Core-mined MNHF signal transaction (`l3/fixtures/mnhf-signals-v24.0.0-rc.4.json`).
#[derive(Clone, Debug)]
pub struct MnhfSignal {
    pub txid: String,
    pub hex: Vec<u8>,
    pub version_bit: u64,
}

/// The MNHF signal transactions Core mined in rc.4's `feature_mnehf.py`, read back from its
/// node's datadir; the fixture's provenance says how.
pub fn mnhf_signals() -> &'static [MnhfSignal] {
    static SIGNALS: std::sync::LazyLock<Vec<MnhfSignal>> = std::sync::LazyLock::new(|| {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../l3/fixtures/mnhf-signals-v24.0.0-rc.4.json"))
                .expect("the MNHF fixture is JSON");
        let signals = fixture["signals"].as_array().expect("the fixture's signals");
        signals
            .iter()
            .map(|s| MnhfSignal {
                txid: s["txid"].as_str().expect("a txid").to_owned(),
                hex: hex_bytes(s["hex"].as_str().expect("a hex")),
                version_bit: s["mnhfTx"]["signal"]["versionBit"].as_u64().expect("a version bit"),
            })
            .collect()
    });
    &SIGNALS
}

fn hex_bytes(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("hex"))
        .collect()
}

/// O10's operators (O10 plan §3.1), chained after [`L3_OPERATORS`]: each id is prefixed `o10.`.
/// O10-2's pairs compose catalogue variants and add none; O10-3 adds the header twins.
pub const O10_OPERATORS: &[Operator] = &[
    // The accept twin of `time_at_mtp`: ContextualCheckBlockHeader refuses `time <= MTP(parent)`
    // (v:4516), so one second past it is accepted.
    Operator {
        name: "o10.time_mtp_edge",
        rejects: &[],
        stage: Stage::Header,
        par: false,
        apply: |p, cx| p.time = cx.mtp + 1,
        variants: &[accept("reg", REG, true)],
    },
    // The accept twin of `time_too_new`: Core refuses `time > now + MAX_FUTURE_BLOCK_TIME`
    // (v:4520, chain.h:24), so `now + 7200` is accepted. `now` is the leg's clock, which both
    // sides hold: `setmocktime` on dashd, `ChainOp::SetTime` on V (`NodeSide::step` sets the
    // SUT's clock, chain/node.rs:669-671).
    Operator {
        name: "o10.time_future_edge",
        rejects: &[],
        stage: Stage::Header,
        par: false,
        apply: |p, cx| p.time = u32::try_from(cx.mocktime + MAX_FUTURE).unwrap_or(u32::MAX),
        variants: &[accept("reg", REG, true)],
    },
    // DEC-1059 (OD-L3-45), pinned, proposal only: generated case 1014's shape, a non-coinbase
    // spend with nVersion 0 and nType 6 (TRANSACTION_QUORUM_COMMITMENT) and no payload, below
    // DIP3. Core (v24.0.0) accepts it: `CQuorumBlockProcessor::ProcessBlock` returns before DIP3
    // (blockprocessor.cpp:218). Its undo does not: `UndoBlock` reads the block's commitments
    // (blockprocessor.cpp:526) by `nType == 6` alone (blockprocessor.cpp:573), so disconnecting
    // the block fails on its missing payload. The block is proposed (TestBlockValidity) and never
    // submitted, so no leg rolls it back; a Core fix that refuses it changes the pinned verdict.
    // The generator refuses the shape (`generator::QC_TYPE_PRE_DIP3`).
    Operator {
        name: "o10.qc_type_pre_dip3",
        rejects: &[],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| {
            let mut tx = spend(cx, 0, 1);
            tx.version = 0;
            tx.tx_type = 6;
            p.txs.push(tx);
        },
        variants: &[accept("reg", REG, true)],
    },
];

/// O10's pinned proposal-only cases (DEC-1059): Proposal only in the o10-gen set, their verdict
/// pinned by their variant's expectation.
pub const PINNED: &[&str] = &["o10.qc_type_pre_dip3"];

/// The header accept twins (O10 plan §3.1): accepted in Submit, Proposal and Header, so their
/// trials add a header-mode trial to a positive case's (`time_at_mtp` and `time_too_new` are their
/// reject controls, in [`HEADER_MODE`]).
pub const HEADER_TWINS: &[&str] = &["o10.time_mtp_edge", "o10.time_future_edge"];

/// Each twin's reject control (the same clock, one second further): `time_at_mtp` sets MTP,
/// `time_too_new` now + 7201.
pub const TWIN_CONTROLS: &[(&str, &str)] =
    &[("o10.time_mtp_edge", "time_at_mtp"), ("o10.time_future_edge", "time_too_new")];

/// `chain-v2`'s bank slots (O2b-4 §3a.2, D55): `MutateX`/`ProposeX` read a slot as a byte modulo
/// this. Fixed for `chain-v2`; a larger bank is a new encoding.
pub const BANK_CAPACITY: u8 = 64;

/// `chain-v2`'s append-only builder bank (O2b-4 §3a.2, D55), reached by `MutateX`/`ProposeX`
/// slot. Builders are only ever appended: a slot at or above its length is refused
/// (`unknown_slot`), so an append changes only inputs that were refused before. Its ids share
/// [`catalogue`]'s namespace.
pub const BANK: &[Operator] = &[];

/// The chain operator index space: [`OPERATORS`]' indices, then [`BANK`]'s after them.
pub fn chain_operator(m: usize) -> &'static Operator {
    OPERATORS.get(m).unwrap_or_else(|| &BANK[m - OPERATORS.len()])
}

/// The chain operator index `slot` selects in `bank` (the generator's is [`BANK`]): `None` for a
/// slot the bank does not fill yet.
pub fn bank_select(slot: u8, bank: &[Operator]) -> Option<usize> {
    let slot = usize::from(slot % BANK_CAPACITY);
    (slot < bank.len()).then(|| OPERATORS.len() + slot)
}

/// The chained catalogue (O10 plan §2.2): [`OPERATORS`], [`L3_OPERATORS`], [`O10_OPERATORS`].
pub const CATALOGUE: &[&[Operator]] = &[OPERATORS, L3_OPERATORS, O10_OPERATORS];

/// Every operator, in [`CATALOGUE`] order.
pub fn catalogue() -> impl Iterator<Item = &'static Operator> {
    CATALOGUE.iter().flat_map(|list| list.iter())
}

/// The operator whose id is `name`.
pub fn by_name(name: &str) -> Option<&'static Operator> {
    catalogue().find(|o| o.name == name)
}

/// A transaction spending the context's coin `i` (or a missing one past the end) into `outs`
/// OP_TRUE outputs, with a 1000-satoshi fee.
pub fn spend(cx: &Cx<'_>, i: usize, outs: usize) -> TxPlan {
    let (prevout, value, script_sig) =
        cx.coins.get(i).map_or((([0xee; 32], i as u32), 100_000, Vec::new()), |c| {
            (c.outpoint, c.value, c.lock.script_sig())
        });
    let each = (value - 1_000) / outs as i64;
    let mut tx = TxPlan::simple(
        vec![Input::new(prevout, script_sig)],
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

    /// `L3_OPERATORS` is a10-5's (pinned at 0560c38ec, whose mutate.rs a10-5's base carries
    /// byte for byte): O10 grows `O10_OPERATORS` instead (O10 plan §2.2).
    #[test]
    fn l3_operators_are_frozen() {
        let names: Vec<&str> = L3_OPERATORS.iter().map(|o| o.name).collect();
        assert_eq!(
            names,
            [
                "control",
                "cb_value",
                "sigops_check",
                "sigops_ctx",
                "sigops_connect",
                "block_size",
                "tx_size_100k",
                "tx_size_1m",
                "payload_size",
                "vin_empty",
                "vout_empty",
                "prevout_null",
                "txouttotal",
                "cb_length",
                "inputvalues",
                "accumulated_fee",
                "spent_earlier",
                "maturity",
                "bip30",
                "p2pkh_bad_sig",
                "p2sh_bad_sig",
                "dersig",
                "bip147",
                "cltv",
                "csv",
                "bip68",
                "bip113",
                "bip113_strict",
                "bad_version",
                "locktime",
                "bip68_blocks",
                "bip68_time",
                "bip68_off",
                "tx_version",
                "tx_version_type",
                "header_version",
                "special_tx_type",
                "special_cb_type",
                "cb_type5",
                "empty_block",
                "v3_type0",
                "m_missing",
                "m_premature",
                "m_in_belowout",
                "m_bip68",
                "m_sigops",
                "m_inputvalues",
                "m_accumulated_fee",
                "m_script_cb_amount",
                "m_script_dip3_gate",
                "m_script_cbtx_payload",
                "s1_mutated_then_honest",
                "s2_child_of_invalid",
                "s3_resubmit",
                "s3h_failed_header_resubmit",
                "s4_proposal_off_tip",
                "s5_unknown_parent",
                "s6_child_of_unmarked_descendant",
            ]
        );
        assert_eq!(L3_OPERATORS.len(), 58);
    }

    /// The catalogue chains the three lists in order, and every O10 id is prefixed `o10.`.
    #[test]
    fn the_catalogue_chains_the_three_lists() {
        let chained: Vec<&str> = catalogue().map(|o| o.name).collect();
        let lists: Vec<&str> = [OPERATORS, L3_OPERATORS, O10_OPERATORS]
            .iter()
            .flat_map(|l| l.iter().map(|o| o.name))
            .collect();
        assert_eq!(chained, lists);
        assert!(O10_OPERATORS.iter().all(|o| o.name.starts_with("o10.")));
        assert!(OPERATORS.iter().chain(L3_OPERATORS).all(|o| !o.name.starts_with("o10.")));
    }

    /// The bank fits its slots, its ids are unique across the catalogue, and slots select
    /// [`BANK`] after [`OPERATORS`].
    #[test]
    fn the_bank_fits_and_selects_after_operators() {
        assert!(BANK.len() <= usize::from(BANK_CAPACITY));
        let mut names: Vec<&str> = catalogue().chain(BANK).map(|o| o.name).collect();
        let n = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), n, "a bank id repeats a catalogue id");
        for slot in 0..=u8::MAX {
            let m = bank_select(slot, BANK);
            assert_eq!(m.is_some(), usize::from(slot % BANK_CAPACITY) < BANK.len(), "{slot}");
            if let Some(m) = m {
                assert_eq!(chain_operator(m).name, BANK[usize::from(slot % BANK_CAPACITY)].name);
            }
        }
        for (m, op) in OPERATORS.iter().enumerate() {
            assert_eq!(chain_operator(m).name, op.name);
        }
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
                for (mode, par, &site) in [Mode::Submit, Mode::Proposal]
                    .into_iter()
                    .flat_map(|m| [Par::One, Par::Four].map(|p| (m, p)))
                    .flat_map(|(m, p)| v.sites.iter().map(move |s| (m, p, s)))
                {
                    {
                        let Expect::Reject {
                            reason,
                            occurrence,
                        } = (v.expect)(mode, par, site)
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
        built_on(op, l3, &[coin(1), coin(2), coin(3)])
    }

    fn built_on(op: &Operator, l3: Option<&L3Cx>, coins: &[Coin]) -> BlockPlan {
        let cx = Cx {
            height: 300,
            mtp: 1_000,
            mocktime: 2_000,
            coins,
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

    /// Group L builds exactly its numbers: the block's legacy sigops, the P2SH spend's accurate
    /// ones on top, the block's size once the leg adds its coinbase tag (no transaction over the
    /// site's limit), a transaction's size, a payload's.
    #[test]
    fn group_l_builds_its_numbers() {
        use dash_consensus::script::sigops::{legacy_sigop_count, script_sigop_count_with_sig};
        let group_l = [
            "sigops_check",
            "sigops_ctx",
            "sigops_connect",
            "block_size",
            "tx_size_100k",
            "tx_size_1m",
            "payload_size",
        ];
        for op in L3_OPERATORS.iter().filter(|o| group_l.contains(&o.name)) {
            for v in op.variants {
                for &site in v.sites {
                    let l3 = L3Cx {
                        trial: *b"trial-l\0",
                        carrier: v.carrier,
                        variant: v.name,
                        site,
                        reward: 5_000,
                        sb_limit: 0,
                        spent: ([0; 32], 0),
                        mature: coin(8),
                        coin_mtp: 900,
                        reuse: &[],
                    };
                    let mut plan = built(op, Some(&l3));
                    plan.txs[0].inputs[0].script_sig.extend(push(b"trial-l\0"));
                    let legacy: u32 =
                        plan.txs.iter().map(|t| legacy_sigop_count(t.tx().raw())).sum();
                    let id = format!("{}.{}@{}", op.name, v.name, site.name());
                    let number = || v.name.parse::<usize>().expect("a number");
                    let last = plan.txs.last().expect("a transaction");
                    match op.name {
                        "sigops_check" | "sigops_ctx" => {
                            assert_eq!(legacy as usize, number(), "{id}")
                        }
                        "sigops_connect" => {
                            let over = u32::from(v.name == "limit+1");
                            let limit = if site == Site::P1 {
                                20_000
                            } else {
                                40_000
                            };
                            let fund = &plan.txs[plan.txs.len() - 2];
                            let p2sh = script_sigop_count_with_sig(
                                &fund.outputs[0].1,
                                &last.inputs[0].script_sig,
                            );
                            assert_eq!((legacy, p2sh), (limit - 13 + over, 13), "{id}");
                        }
                        "block_size" => {
                            assert_eq!(plan.build().bytes.len(), number(), "{id}");
                            let cap = if site == Site::P1 {
                                1_000_000
                            } else {
                                100_000
                            };
                            assert!(plan.txs.iter().all(|t| tx_size(t) <= cap), "{id}");
                            assert_eq!(legacy, 0, "{id}");
                        }
                        "tx_size_100k" | "tx_size_1m" => {
                            assert_eq!((plan.txs.len(), tx_size(last)), (2, number()), "{id}")
                        }
                        "payload_size" => {
                            assert_eq!(
                                (last.version, last.extra_payload.len()),
                                (3, number()),
                                "{id}"
                            )
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    /// Groups S and I build their shapes: the coinbase scriptSig at the variant's length behind
    /// the height push, MAX_MONEY (+1) across two outputs, a null prevout beside a real one, the
    /// MAX_MONEY coins spent, the repeated setup transaction, the spent and mature coins.
    #[test]
    fn groups_s_and_i_build_their_shapes() {
        let base =
            TxPlan::simple(vec![Input::new(([4; 32], 0), Vec::new())], vec![(7, vec![OP_RETURN])]);
        let spent_base =
            TxPlan::simple(vec![Input::new(([5; 32], 0), Vec::new())], vec![(8, vec![OP_RETURN])]);
        let reuse = [base.clone(), spent_base.clone()];
        let build = |op: &str, variant: &'static str, site: Site, height: u32| {
            let op = by_name(op).expect("an operator");
            let max = |n: u8| Coin {
                value: MAX_MONEY,
                ..coin(n)
            };
            let coins = if max_money_coins(op.name, variant, site) > 0 {
                [max(1), max(2), coin(3)]
            } else {
                [coin(1), coin(2), coin(3)]
            };
            let v = op.variants.iter().find(|v| v.name == variant).expect("a variant");
            let l3 = L3Cx {
                trial: *b"trial-s\0",
                carrier: v.carrier,
                variant,
                site,
                reward: 5_000,
                sb_limit: 0,
                spent: ([6; 32], 1),
                mature: coin(8),
                coin_mtp: 900,
                reuse: &reuse,
            };
            let cx = Cx {
                height,
                mtp: 1_000,
                mocktime: 2_000,
                coins: &coins,
                immature: Some(coin(9)),
                l3: Some(&l3),
            };
            let mut plan = BlockPlan {
                prev: [0; 32],
                version: 0x2000_0000,
                time: 1_500,
                bits: REGTEST_BITS,
                txs: vec![coinbase(height, 5_000, 1)],
                merkle_override: None,
            };
            (op.apply)(&mut plan, &cx);
            (plan, coins)
        };
        for (variant, site, height, len) in [
            ("1", Site::Reg, 300, 1),
            ("2", Site::P1, 115, 2),
            ("100", Site::Reg, 300, 100),
            ("101", Site::Reg, 300, 101),
        ] {
            let (plan, _) = build("cb_length", variant, site, height);
            let sig = &plan.txs[0].inputs[0].script_sig;
            assert_eq!(sig.len(), len, "cb_length.{variant}");
            if len > 1 {
                assert!(sig.starts_with(&script_num(i64::from(height))), "cb_length.{variant}");
            }
            assert_eq!(plan.txs.len(), 2);
        }
        for (variant, total) in [("max", MAX_MONEY), ("max+1", MAX_MONEY + 1)] {
            let (plan, _) = build("txouttotal", variant, Site::Reg, 300);
            let out = &plan.txs[1].outputs;
            assert_eq!((out.len(), out.iter().map(|o| o.0).sum::<i64>()), (2, total));
            assert!(out.iter().all(|o| o.0 <= MAX_MONEY));
        }
        // OD-L3-23 (b): at BO the MAX_MONEY outputs spend a minted MAX_MONEY coin, fee-free.
        let (plan, coins) = build("txouttotal", "max", Site::BO, 1_200);
        assert_eq!(coins[0].value, MAX_MONEY);
        assert_eq!(fees(&plan, &coins), 0);
        assert_eq!(label("txouttotal", "max", Site::Reg), Some("control: tc:46 passed"));
        assert_eq!(label("txouttotal", "max", Site::BO), None);
        let (plan, _) = build("prevout_null", "reg", Site::Reg, 300);
        assert_eq!(plan.txs[1].inputs[1].prevout, ([0; 32], u32::MAX));
        assert_ne!(plan.txs[1].inputs[0].prevout, ([0; 32], u32::MAX));
        let (plan, coins) = build("inputvalues", "2max", Site::BO, 1_200);
        assert_eq!(
            plan.txs[1].inputs.iter().map(|i| i.prevout).collect::<Vec<_>>(),
            [coins[0].outpoint, coins[1].outpoint]
        );
        for (variant, n) in [("max", 1), ("2max", 2)] {
            let (plan, coins) = build("accumulated_fee", variant, Site::BO, 1_200);
            assert_eq!(fees(&plan, &coins), n * MAX_MONEY, "accumulated_fee.{variant}");
        }
        for (variant, repeated) in [("unspent", &base), ("spent", &spent_base)] {
            let (plan, _) = build("bip30", variant, Site::Reg, 300);
            assert_eq!(&plan.txs[1], repeated, "bip30.{variant}");
        }
        let (plan, _) = build("spent_earlier", "reg", Site::Reg, 300);
        assert_eq!(plan.txs[1].inputs[0].prevout, ([6; 32], 1));
        for (variant, spent) in [("99", coin(9)), ("100", coin(8))] {
            let (plan, _) = build("maturity", variant, Site::Reg, 300);
            assert_eq!(plan.txs[1].inputs[0].prevout, spent.outpoint, "maturity.{variant}");
        }
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
                variant: "reg",
                site: Site::Reg,
                reward: 0,
                sb_limit: 0,
                spent: ([0; 32], 0),
                mature: coin(8),
                coin_mtp: 900,
                reuse: &[],
            };
            let b = L3Cx {
                trial: *b"trial-b\0",
                carrier,
                variant: "reg",
                site: Site::Reg,
                reward: 0,
                sb_limit: 0,
                spent: ([0; 32], 0),
                mature: coin(8),
                coin_mtp: 900,
                reuse: &[],
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

    /// Group B's scripts pass without their rule's flag and fail with its error with it
    /// (dash-consensus's interpreter, P2SH on as on every Dash block; no transaction, so every
    /// signature and lock check fails as in Core's `BaseSignatureChecker`).
    #[test]
    fn group_b_scripts_switch_with_their_flag() {
        use dash_consensus::script::{NoSignatureChecker, ScriptError, ScriptFlags, verify_script};
        for (lock, flag, error) in [
            (Lock::NonDerNot, ScriptFlags::DERSIG, ScriptError::SigDer),
            (Lock::NullDummy, ScriptFlags::NULLDUMMY, ScriptError::SigNullDummy),
            (Lock::Cltv(140), ScriptFlags::CHECKLOCKTIMEVERIFY, ScriptError::UnsatisfiedLocktime),
            (Lock::Csv(10), ScriptFlags::CHECKSEQUENCEVERIFY, ScriptError::UnsatisfiedLocktime),
        ] {
            let run = |flags| {
                verify_script(&lock.script_sig(), &lock.script_pubkey(), flags, &NoSignatureChecker)
            };
            assert_eq!(run(ScriptFlags::P2SH), Ok(()), "{lock:?} without its flag");
            assert_eq!(run(ScriptFlags::P2SH.union(flag)), Err(error), "{lock:?} with its flag");
        }
    }

    /// Group B runs each family below its activation height (accepted) and at it (refused, but
    /// `bip113_strict`, refused on both); the time locks sit where m-4 puts them against the
    /// parent's MTP and the block's time, the versions and the BIP68 lock are as named.
    #[test]
    fn group_b_builds_its_edges() {
        let (mtp, time) = (1_000, 1_500);
        let group_b =
            ["dersig", "bip147", "cltv", "csv", "bip68", "bip113", "bip113_strict", "bad_version"];
        for op in L3_OPERATORS.iter().filter(|o| group_b.contains(&o.name)) {
            for v in op.variants {
                let [Site::At(below), Site::At(at)] = v.sites else {
                    panic!("{}.{}: not one height and the next", op.name, v.name);
                };
                assert_eq!(below + 1, *at, "{}.{}", op.name, v.name);
                assert!([DERSIG_HEIGHT, CLTV_HEIGHT, CSV_HEIGHT, BIP147_HEIGHT].contains(at));
                for (mode, par) in [(Mode::Submit, Par::One), (Mode::Proposal, Par::Four)] {
                    let below_expect = (v.expect)(mode, par, Site::At(*below));
                    let at_expect = (v.expect)(mode, par, Site::At(*at));
                    assert_eq!(below_expect == Expect::Accept, op.name != "bip113_strict");
                    assert!(matches!(at_expect, Expect::Reject { .. }), "{}", op.name);
                }
                for &site in v.sites {
                    let Site::At(height) = site else {
                        unreachable!()
                    };
                    let coins = [coin(1), coin(2), coin(3)];
                    let l3 = L3Cx {
                        trial: *b"trial-b\0",
                        carrier: v.carrier,
                        variant: v.name,
                        site,
                        reward: 5_000,
                        sb_limit: 0,
                        spent: ([6; 32], 1),
                        mature: coin(8),
                        coin_mtp: 900,
                        reuse: &[],
                    };
                    let cx = Cx {
                        height,
                        mtp,
                        mocktime: i64::from(time),
                        coins: &coins,
                        immature: Some(coin(9)),
                        l3: Some(&l3),
                    };
                    let mut plan = BlockPlan {
                        prev: [0; 32],
                        version: 0x2000_0000,
                        time,
                        bits: REGTEST_BITS,
                        txs: vec![coinbase(height, 5_000, 1)],
                        merkle_override: None,
                    };
                    (op.apply)(&mut plan, &cx);
                    let last = plan.txs.last().expect("a transaction");
                    match op.name {
                        "bip113" => {
                            assert_eq!(last.lock_time, time - 1);
                            assert!(mtp <= last.lock_time);
                        }
                        "bip113_strict" => assert_eq!(
                            last.lock_time,
                            if height >= CSV_HEIGHT {
                                mtp
                            } else {
                                time
                            }
                        ),
                        "bad_version" => assert_eq!(plan.version.to_string(), v.name),
                        "bip68" => {
                            assert_eq!((last.version, last.inputs[0].sequence), (2, 1000))
                        }
                        _ => assert_eq!(plan.txs.len(), 3, "{}: a locked spend", op.name),
                    }
                    if op.name.starts_with("bip113") {
                        assert_eq!(last.inputs[0].sequence, 0xffff_fffe);
                    }
                }
            }
        }
    }

    /// Group X's version and type rows read back from their bytes as Core reads them
    /// (`PackVersionAndType`, primitives/transaction.h:237-240 and :366-367 at v24.0.0; a payload
    /// only from version 3): version -1 fills the low half alone and keeps the type.
    #[test]
    fn group_x_bytes_read_back_as_core_reads_them() {
        for (op, variant, index, version, tx_type, payload) in [
            ("tx_version", "-1", 1, -1, 0, 0),
            ("tx_version_type", "-1", 1, -1, 5, 0),
            ("tx_version_type", "0", 1, 0, 5, 0),
            ("tx_version_type", "2", 1, 2, 5, 0),
            ("special_tx_type", "13", 1, 3, 13, 1),
            ("special_cb_type", "1", 0, 3, 1, 1),
            ("cb_type5", "1", 0, 1, 5, 0),
            ("cb_type5", "2", 0, 2, 5, 0),
            ("v3_type0", "reg", 1, 3, 0, 0),
        ] {
            let op = by_name(op).expect("an operator");
            let v = op.variants.iter().find(|v| v.name == variant).expect("a variant");
            let l3 = L3Cx {
                trial: *b"trial-x\0",
                carrier: v.carrier,
                variant,
                site: Site::Reg,
                reward: 5_000,
                sb_limit: 0,
                spent: ([6; 32], 1),
                mature: coin(8),
                coin_mtp: 900,
                reuse: &[],
            };
            let plan = built(op, Some(&l3));
            let back = BlockPlan::from_block(&plan.build().bytes).expect("reads back");
            let tx = &back.txs[index];
            assert_eq!(
                (tx.version, tx.tx_type, tx.extra_payload.len()),
                (version, tx_type, payload),
                "{}.{variant}",
                op.name
            );
        }
        let header = |name| {
            let op = by_name("header_version").expect("an operator");
            let v = op.variants.iter().find(|v| v.name == name).expect("a variant");
            let l3 = L3Cx {
                trial: *b"trial-x\0",
                carrier: v.carrier,
                variant: name,
                site: Site::Reg,
                reward: 5_000,
                sb_limit: 0,
                spent: ([6; 32], 1),
                mature: coin(8),
                coin_mtp: 900,
                reuse: &[],
            };
            format!("bad-version(0x{:08x})", built(op, Some(&l3)).version)
        };
        for name in ["0x00000000", "0x80000000", "0x00000001"] {
            assert_eq!(header(name), format!("bad-version({name})"));
        }
    }

    /// Group M puts F's failing transaction and the script fault in the variant's order (one
    /// transaction for `same`), the fault's transactions fee-free; the r7 rows build their
    /// second fault.
    /// Group P: every scenario run has steps, named uniquely, full set only at Reg; each
    /// step's reject is one its operator lists, at an occurrence whose template is the reason;
    /// the variant expects its last step's answer.
    #[test]
    fn scenarios_declare_their_steps() {
        let scenarios: Vec<&Operator> = L3_OPERATORS
            .iter()
            .filter(|o| matches!(o.name.as_bytes(), [b's', b'1'..=b'6', ..]))
            .collect();
        assert_eq!(scenarios.len(), 7);
        for op in scenarios {
            for v in op.variants {
                let id = format!("{}.{}", op.name, v.name);
                let steps = scenario_steps(op.name, v.name).unwrap_or_else(|| panic!("{id}"));
                assert!(v.full_only && v.sites == REG, "{id}");
                let mut names: Vec<&str> = steps.iter().map(|s| s.0).collect();
                names.sort_unstable();
                names.dedup();
                assert_eq!(names.len(), steps.len(), "{id}: a step name repeats");
                for (name, _, want) in steps {
                    if let Expect::Reject {
                        reason,
                        occurrence,
                    } = want
                    {
                        assert!(op.rejects.contains(reason), "{id}+{name}: {reason}");
                        assert!(occurrence.ends_with(&format!(":{reason}")), "{id}+{name}");
                    }
                }
                let last = steps.last().expect("a step").2;
                assert_eq!((v.expect)(Mode::Submit, Par::One, Site::Reg), last, "{id}");
            }
        }
        assert_eq!(scenario_steps("cb_value", "bound+1"), None);
    }

    #[test]
    fn group_m_bo_families_pass_check_transaction() {
        // The leg funds these from its MAX_MONEY mints (`max_money_coins`): no transaction of
        // theirs may fail CheckTransaction before CheckTxInputs or the fee sum (c7-full, 10-31:
        // `m_inputvalues.same` paid the fault's input on top of MAX_MONEY).
        let max = Coin {
            value: MAX_MONEY,
            ..coin(1)
        };
        let coins = [
            max,
            Coin {
                outpoint: ([2; 32], 0),
                ..max
            },
            coin(3),
        ];
        for name in ["m_inputvalues", "m_accumulated_fee"] {
            let op = by_name(name).expect("an operator");
            for v in op.variants {
                assert_eq!(max_money_coins(name, v.name, Site::BO), 2, "{name}.{}", v.name);
                let l3 = L3Cx {
                    trial: *b"trial-m\0",
                    carrier: v.carrier,
                    variant: v.name,
                    site: Site::BO,
                    reward: 5_000,
                    sb_limit: 0,
                    spent: ([6; 32], 1),
                    mature: coin(8),
                    coin_mtp: 900,
                    reuse: &[],
                };
                let plan = built_on(op, Some(&l3), &coins);
                // Nor may the fee family fail CheckTxInputs#3 before the fee sum (f1-full, 10-31:
                // `m_accumulated_fee.same` put the fault's input beside a MAX_MONEY one).
                let mut values: std::collections::HashMap<([u8; 32], u32), i64> =
                    coins.iter().chain([&l3.mature]).map(|c| (c.outpoint, c.value)).collect();
                for tx in &plan.txs {
                    let txid = tx.txid();
                    values.extend((0u32..).zip(&tx.outputs).map(|(n, o)| ((txid, n), o.0)));
                }
                for tx in &plan.txs[1..] {
                    let checked = dash_consensus::tx::check_transaction(&tx.tx());
                    assert!(checked.is_ok(), "{name}.{}: {checked:?}", v.name);
                    let input: i64 = tx
                        .inputs
                        .iter()
                        .map(|i| values.get(&i.prevout).copied().unwrap_or(0))
                        .sum();
                    let fees = name == "m_accumulated_fee";
                    assert!(
                        !fees || input <= MAX_MONEY,
                        "{name}.{}: inputs sum to {input}",
                        v.name
                    );
                }
            }
        }
    }

    #[test]
    fn group_m_orders_its_faults() {
        type Failing = fn(&TxPlan) -> bool;
        let fails: [(&str, Failing); 7] = [
            ("m_missing", |t| t.inputs[0].prevout.0 == [0xee; 32]),
            ("m_premature", |t| t.inputs[0].prevout == coin(9).outpoint),
            ("m_in_belowout", |t| t.inputs[0].prevout == coin(1).outpoint),
            ("m_bip68", |t| t.inputs[0].prevout == coin(1).outpoint),
            ("m_sigops", |t| t.inputs[0].script_sig == Lock::Sigops(SIGOPS_MAX).script_sig()),
            ("m_inputvalues", |t| t.inputs.len() >= 2 && t.inputs[1].prevout == coin(2).outpoint),
            // In `same` the second fee is the fault's input alone, all of it fee.
            ("m_accumulated_fee", |t| {
                t.inputs[0].prevout == coin(2).outpoint
                    || (t.inputs[0].script_sig == Lock::False.script_sig() && t.outputs[0].0 == 0)
            }),
        ];
        for (name, failing) in fails {
            let op = by_name(name).expect("an operator");
            for v in op.variants {
                let l3 = L3Cx {
                    trial: *b"trial-m\0",
                    carrier: v.carrier,
                    variant: v.name,
                    site: v.sites[0],
                    reward: 5_000,
                    sb_limit: 0,
                    spent: ([6; 32], 1),
                    mature: coin(8),
                    coin_mtp: 900,
                    reuse: &[],
                };
                let plan = built(op, Some(&l3));
                let id = format!("{name}.{}", v.name);
                let at = |pred: &dyn Fn(&TxPlan) -> bool| {
                    let found: Vec<usize> =
                        (1..plan.txs.len()).filter(|&i| pred(&plan.txs[i])).collect();
                    assert_eq!(found.len(), 1, "{id}: {found:?}");
                    found[0]
                };
                let f = at(&failing);
                let script = at(&|t: &TxPlan| {
                    t.inputs.iter().any(|i| i.script_sig == Lock::False.script_sig())
                });
                let fund = at(&|t: &TxPlan| t.inputs[0].prevout == coin(8).outpoint);
                assert_eq!(plan.txs[fund].outputs[0], (coin(8).value, Lock::False.script_pubkey()));
                assert!(fund < script, "{id}");
                match v.name {
                    "script_first" => assert!(script < f, "{id}"),
                    "f_first" => assert!(f < script, "{id}"),
                    _ => assert_eq!(f, script, "{id}"),
                }
                if name == "m_in_belowout" {
                    let tx = &plan.txs[f];
                    let input = coin(1).value
                        + if f == script {
                            coin(8).value
                        } else {
                            0
                        };
                    assert_eq!(tx.outputs[0].0, input + 1, "{id}");
                }
            }
        }
        let build = |name: &str, variant: &'static str| {
            let op = by_name(name).expect("an operator");
            let l3 = L3Cx {
                trial: *b"trial-m\0",
                carrier: Carrier::OpReturn,
                variant,
                site: Site::Reg,
                reward: 5_000,
                sb_limit: 0,
                spent: ([6; 32], 1),
                mature: coin(8),
                coin_mtp: 900,
                reuse: &[],
            };
            built(op, Some(&l3))
        };
        let plan = build("m_script_cb_amount", "reg");
        assert_eq!(plan.txs[0].outputs[0].0, 5_001);
        assert_eq!(fees(&plan, &[coin(8)]), 0, "the fault pays no fee");
        for (variant, gate_index) in [("script_first", 3), ("gate_first", 1)] {
            let plan = build("m_script_dip3_gate", variant);
            assert_eq!((plan.txs[gate_index].version, plan.txs[gate_index].tx_type), (3, 1));
        }
        let plan = build("m_script_cbtx_payload", "reg");
        assert_eq!((plan.txs[0].version, plan.txs[0].tx_type, plan.txs.len()), (1, 5, 3));
        // The proposal-only row: a block of no transactions still builds and reads back.
        let empty = build("empty_block", "reg");
        let back = BlockPlan::from_block(&empty.build().bytes).expect("reads back");
        assert!(empty.txs.is_empty() && back.txs.is_empty());
    }
}
