//! One L3 leg (plan §3.2, §3.3): a dashd and V on the same chain arguments, the setup chain
//! both build, each case's trials on both, and the planes after every op. Every op that changes
//! a node is logged with dashd's reply and V's step, for the shim replay ([`super::replay`]).
//! The planes' decisions are [`super::planes`]'s; the leg fetches what they read.
//!
//! The leg stops at its first harness error or divergence, which its log names; nothing after
//! it is scored.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use dash_oracle_sys::{ChainOp, Pin, Post, RpcError};
use serde_json::{Value, json};

use dashd_chainstate::evo::gates;

use super::dashd::{Process, is_synced};
use super::evo;
use super::inventory::{Binding, Effects, Facts, Inventory, ParentState, Reach};
use super::ledger::{self, Ledger, Mempool, Removal, is_coinbase, muhash_refs};
use super::model::{self, Model};
use super::planes::{self, View, block_at, rpc_hash};
use super::trial::{CaseId, Kind, Owner, TrialId, TxidLedger, from_hex, hex, to_hex};
use super::version::{Dashd, private_dir};
use crate::chain::build::TxPlan;
use crate::chain::build::{self, BlockPlan, Built, Lock, push};
use crate::chain::mutate::{
    BIP147_HEIGHT, BO_BASE, BUDGET_START, CLTV_HEIGHT, COINBASE_TAG, CSV_HEIGHT, Cx, DERSIG_HEIGHT,
    DIP0001_HEIGHT, EXACT_COINBASE, Expect, HEADER_MODE, L3_OPERATORS, L3Cx, MAX_MONEY, Mode,
    Operator, P1_BASE, PROPOSAL_ONLY, Par, REUSE_SETUP, ScenarioStep, Site, Variant, catalogue,
    max_money_coins, scenario_steps,
};
use crate::chain::node::{NodeSide, index_digest};
use crate::chain::params;
use crate::chain::shape::Outcome;
use crate::chain::step::Step;
use crate::chain::world::{Coin, Hash, header_hash};

pub mod av;

/// The legs' first clock; height `h`'s is `T0 + 150·h` (plan §3.2).
pub const T0: i64 = 1_700_000_000;
/// The height of the first `Reg` case: every boundary passed.
pub const CASE_BASE: u32 = 201;
/// The split's height (plan §3.2's setup txs): every case's three coins come from the split
/// transactions of the setup block here, each spending the next coinbase from height 1, so every
/// later coinbase stays unspent (the maturity and BIP30 rows read them).
pub const SPLIT_HEIGHT: u32 = 101;
const _: () = assert!(SPLIT_HEIGHT > build::MATURITY, "the split spends mature coinbases");
const _: () = assert!(SPLIT_HEIGHT < P1_BASE, "every case is funded before it");
/// Cases per split transaction (three outputs each).
const SPLIT_CASES: usize = 300;
/// What each setup transaction leaves its block's coinbase.
const SETUP_FEE: i64 = 1_000;

/// D49's arguments, then the boundary heights (main legs, Q2).
pub const CHAIN_ARGS: [&str; 8] = [
    "-dip3params=2000:2000",
    "-testactivationheight=v20@2000",
    "-testactivationheight=mn_rr@2000",
    "-testactivationheight=dip0001@120",
    "-testactivationheight=dersig@130",
    "-testactivationheight=cltv@140",
    "-testactivationheight=csv@150",
    "-testactivationheight=bip147@160",
];
/// The boundary heights: P6 runs on both sides of each.
pub const BOUNDARIES: [u32; 5] =
    [DIP0001_HEIGHT, DERSIG_HEIGHT, CLTV_HEIGHT, CSV_HEIGHT, BIP147_HEIGHT];

/// `RPC_INVALID_ADDRESS_OR_KEY`: an unknown block.
const UNKNOWN_BLOCK: i64 = -5;
/// A raw type-5 transaction (the coinbase type CbTx's step 4 skips at `vtx[0]`).
const TRANSACTION_COINBASE: u16 = 5;

/// A leg's chain arguments: the main legs' and its `-par`.
pub fn chain_args(par: Par) -> Vec<String> {
    CHAIN_ARGS.iter().copied().chain([par.arg()]).map(str::to_owned).collect()
}

/// A case of the run.
#[derive(Clone, Copy)]
pub struct Case {
    pub id: CaseId,
    pub op: &'static Operator,
    pub variant: &'static Variant,
    /// The height its block is built at (on parent `height - 1`).
    pub height: u32,
}

impl Case {
    /// Whether the case is positive: its cold submit is accepted, so it has a rollback and a
    /// T-seq (plan §3.2).
    pub fn positive(&self) -> bool {
        self.expect(Mode::Submit) == Expect::Accept
    }

    /// Whether the case's funding is standard (P2SH of `OP_TRUE`) rather than bare `OP_TRUE`: L3's
    /// own operators, whose transactions dashd's mempool must take back at a rollback (a10-4 review
    /// r2-m1). The shared operators keep their bytes. By name, unique across the catalogue: a
    /// `const` slice has no one address to compare.
    pub fn standard_funding(&self) -> bool {
        L3_OPERATORS.iter().any(|o| o.name == self.op.name)
    }

    /// What Core answers the case's block in `mode`.
    pub fn expect(&self, mode: Mode) -> Expect {
        (self.variant.expect)(mode, self.id.par, self.id.site)
    }

    /// Its trials in `set`: a named scenario's steps, or the case's kinds (plan §3.4: header
    /// mode in the full set only).
    pub fn kinds(&self, set: Set) -> Vec<Kind> {
        if let Some(steps) = scenario_steps(self.id.op, self.id.variant) {
            steps.iter().map(|s| Kind::Step(s.0, s.1)).collect()
        } else if PROPOSAL_ONLY.contains(&self.op.name) {
            vec![Kind::Prop]
        } else if self.positive() {
            vec![Kind::Cold, Kind::Prop, Kind::Seq]
        } else if HEADER_MODE.contains(&self.op.name) && set == Set::Full {
            vec![Kind::Cold, Kind::Prop, Kind::Header]
        } else {
            vec![Kind::Cold, Kind::Prop]
        }
    }
}

/// The cases a run builds (plan §3.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Set {
    /// The tier-1 sites ([`Site::tier1`]).
    Tier1,
    /// Every site.
    Full,
}

impl Set {
    pub fn parse(name: &str) -> Result<Set, String> {
        match name {
            "tier1" => Ok(Set::Tier1),
            "full" => Ok(Set::Full),
            _ => Err(format!("L3_SET={name}: not tier1 or full")),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Set::Tier1 => "tier1",
            Set::Full => "full",
        }
    }

    /// Whether the set runs `variant` at `site`.
    pub fn runs(self, variant: &Variant, site: Site) -> bool {
        self == Set::Full || (!variant.full_only && site.tier1())
    }
}

/// The set's cases on a leg, in manifest order, which fixes each case's height and funding
/// whatever the filter: a superblock site's cases at its height, every other site's one per
/// height of the site in order, from [`CASE_BASE`] (`Reg`) or the budget start. `Err` for a
/// site whose window ends before its next case's height (OD-L3-72 n4).
pub fn cases(set: Set, par: Par) -> Result<Vec<Case>, String> {
    let mut next: BTreeMap<Site, u32> = BTreeMap::new();
    let mut out = Vec::new();
    for op in catalogue() {
        for variant in op.variants {
            for &site in variant.sites.iter().filter(|s| set.runs(variant, **s)) {
                let height = match site {
                    Site::SB(h) | Site::At(h) => h,
                    _ => {
                        let first = match site {
                            Site::P1 => P1_BASE,
                            Site::Reg => CASE_BASE,
                            Site::BO => BO_BASE,
                            _ => BUDGET_START,
                        };
                        let h = next.entry(site).or_insert(first);
                        let height = next_height(site, *h)?;
                        *h = height + 1;
                        height
                    }
                };
                out.push(Case {
                    id: CaseId {
                        op: op.name,
                        variant: variant.name,
                        site,
                        par,
                    },
                    op,
                    variant,
                    height,
                });
            }
        }
    }
    Ok(out)
}

/// The first height from `from` on `site`, or `Err` once the search passes the site's window
/// ([`Site::end`]) rather than running on.
fn next_height(site: Site, from: u32) -> Result<u32, String> {
    (from..site.end()).find(|h| site.holds(*h)).ok_or_else(|| {
        format!(
            "site {}: no height from {from} holds it before its window's end {}",
            site.name(),
            site.end()
        )
    })
}

/// Each case's mints, by case index.
pub type Mints = BTreeMap<usize, Vec<usize>>;

/// The `MAX_MONEY` coins the set's cases spend (rev1 §4's notes: the only way to reach tv:190
/// and v:2731 on regtest): each case's mints, in manifest order, and the mints' heights, the old
/// budget window's from its start (Core does not check a coinbase's value there, `NoCheck`), each
/// mature before [`BO_BASE`].
pub fn mints(cases: &[Case]) -> Result<(Mints, Vec<u32>), String> {
    let mut next = 0;
    let mut of = BTreeMap::new();
    for (i, c) in cases.iter().enumerate() {
        let n = max_money_coins(c.id.op, c.id.variant, c.id.site);
        if n > 0 {
            of.insert(i, (next..next + n).collect());
            next += n;
        }
    }
    let heights: Vec<u32> =
        (BUDGET_START..BO_BASE).filter(|h| Site::BW.holds(*h)).take(next).collect();
    if heights.len() != next || heights.last().is_some_and(|h| h + build::MATURITY > BO_BASE) {
        return err(format!("{next} mints do not fit the budget window before {BO_BASE}"));
    }
    Ok((of, heights))
}

/// The lock a case's split coins carry ([`Case::standard_funding`]'s).
fn case_lock(case: &Case) -> Lock {
    if case.standard_funding() {
        Lock::P2shTrue
    } else {
        Lock::True
    }
}

/// One op both nodes ran.
#[derive(Clone, Debug)]
pub struct Logged {
    pub op: ChainOp,
    pub trial: Option<TrialId>,
    /// dashd's reply (`SETTIME`'s is null).
    pub dashd: Outcome,
    pub v: Step,
    /// The model's P7 fields after the op, where P7 ran on it (the replay's harness
    /// cross-check, OD-L3-35).
    pub model_p7: Option<model::P7>,
}

/// A trial's record.
#[derive(Clone, Debug)]
pub struct TrialRecord {
    pub trial: TrialId,
    pub hash: Hash,
    /// The credited op's index in the log.
    pub op: usize,
    pub mode: Mode,
    pub reply: Outcome,
    /// `None` for an accept or an RPC refusal (no occurrence).
    pub binding: Option<Binding>,
    /// The bound row's class (§3.5).
    pub bound_class: Option<String>,
    /// V's class: its `BlockChecked`'s (submit) or its TBV state's (proposal).
    pub v_class: Option<String>,
    pub effects: Effects,
    /// §3.6's per-body observations, with the credited op's E-evo delta (M-3).
    pub evo: Value,
    /// Disconnected transactions dashd's ATMP declined at the rollback, with its reason.
    pub declined: Listed,
    /// Transactions the submitted block took from dashd's mempool, with their reason
    /// (OD-L3-21: `confirmed`, `conflict on <outpoint>`, `descendant of <txid>`).
    pub removed: Listed,
    /// Rolled-back transactions dashd's mempool took back at the rollback (r2-m1's gain path).
    pub gained: Vec<String>,
}

/// A leg's result.
#[derive(Debug)]
pub struct LegLog {
    pub name: String,
    pub set: Set,
    pub par: Par,
    pub chain_args: Vec<String>,
    pub dashd_argv: Vec<String>,
    pub ops: Vec<Logged>,
    pub trials: Vec<TrialRecord>,
    /// V's load status at construction (OD-L3-16): `None` once it serves.
    pub load_refused: Option<String>,
    /// V's E-evo counters after the last op (§3.6).
    pub evo_counts: BTreeMap<&'static str, u64>,
    /// What the planes read after each trial op, when the run records (`L3_RECORD`): the
    /// witnesses' fixtures, with Core's post added by the replay.
    pub recorded: Vec<Value>,
    /// The AV fixture the leg ran (plan §7.4), and an AV-on fixture's header checks.
    pub fixture: Option<av::Defect>,
    pub av: Option<Value>,
    /// The leg's wall time.
    pub seconds: f64,
    /// The leg's first harness error or divergence; `None` for a leg done.
    pub error: Option<String>,
}

/// What a leg runs.
pub struct LegConfig<'a> {
    pub dashd: &'a Dashd,
    pub set: Set,
    pub par: Par,
    pub inventory: &'a Inventory,
    /// Run only the cases whose id or operator is this (plan §3.2's filtered run).
    pub only: Option<&'a str>,
    /// Record what the planes read after each trial op ([`LegLog::recorded`]).
    pub record: bool,
    /// An AV fixture instead of the set's cases (plan §7.4).
    pub fixture: Option<&'a av::Fixture>,
}

/// Run a leg in a new directory under `dir`.
pub fn run(config: &LegConfig<'_>, dir: &Path) -> LegLog {
    let mut args = chain_args(config.par);
    args.extend(config.fixture.and_then(av::Fixture::assumevalid_arg));
    run_on(config, dir, args)
}

/// [`run`] on the given chain arguments.
fn run_on(config: &LegConfig<'_>, dir: &Path, chain_args: Vec<String>) -> LegLog {
    let mut log = LegLog {
        name: config.fixture.map_or_else(|| config.par.name().to_owned(), av::Fixture::name),
        set: config.set,
        par: config.par,
        chain_args,
        dashd_argv: Vec::new(),
        ops: Vec::new(),
        trials: Vec::new(),
        load_refused: None,
        evo_counts: BTreeMap::new(),
        recorded: Vec::new(),
        fixture: config.fixture.map(|f| f.defect),
        av: None,
        seconds: 0.0,
        error: None,
    };
    let start = std::time::Instant::now();
    let mut leg = match Leg::open(config, dir, &mut log) {
        Ok(leg) => leg,
        Err(e) => {
            log.error = Some(e);
            return log;
        }
    };
    if let Err(e) = leg.chain(&mut log) {
        log.error = Some(e);
    }
    if let Err(e) = leg.node.take().map_or(Ok(()), Process::stop) {
        log.error.get_or_insert(e);
    }
    log.seconds = start.elapsed().as_secs_f64();
    log
}

/// A trial's block on the tip before its op ([`Leg::prepare`]).
struct Prepared {
    plan: BlockPlan,
    /// The parent's median time past.
    mtp: u32,
    /// Its funding, then the immature and mature coinbases: what the manifest checks.
    coins: Vec<Coin>,
    /// The coin setup spent at the split (`spent_earlier`'s).
    spent: (Hash, u32),
}

/// One op of a trial or scenario step ([`Leg::execute`]).
struct Op<'p> {
    trial: TrialId,
    plan: &'p BlockPlan,
    built: &'p Built,
    mode: Mode,
    want: Expect,
    /// The block's height and parent, and the parent's median time past.
    height: u32,
    prev: Hash,
    mtp: u32,
}

/// An accepted submit's rollback, not yet run ([`Leg::rollback`]).
struct Pending {
    trial: TrialId,
    /// The submit's index in the leg's trial records.
    record: usize,
    plan: BlockPlan,
    hash: Hash,
    txids: Vec<Hash>,
    own: Vec<Hash>,
    prev: Hash,
    gates: BodyGates,
    baseline: (Ledger, Mempool, BTreeSet<String>),
}

/// The parent guard of AcceptBlockHeader's walk of the failed set (v:4699).
const WALKED: &str = "unmarked_failed_ancestor";

/// A coinbase-only block at `height` on `prev`, with `template`'s header fields and `trial`'s
/// tag: the blocks a scenario builds off its first one. None of them is ever connected, so its
/// coinbase pays nothing.
fn child(template: &BlockPlan, prev: Hash, height: u32, trial: TrialId) -> BlockPlan {
    BlockPlan {
        prev,
        txs: vec![build::coinbase(height, 0, u64::from_le_bytes(trial.bytes()))],
        merkle_override: None,
        ..template.clone()
    }
}

struct Leg<'a> {
    config: &'a LegConfig<'a>,
    node: Option<Process>,
    v: NodeSide,
    model: Model,
    ledger: Ledger,
    mempool: Mempool,
    txids: TxidLedger,
    cases: Vec<Case>,
    /// Each case's three coins, once the split confirmed.
    coins: BTreeMap<usize, Vec<Coin>>,
    /// The cases [`mints`] funds, with their mints, and the mints' heights.
    minted_for: Mints,
    mint_heights: Vec<u32>,
    /// The setup transactions a `REUSE_SETUP` case repeats: `T_base` (the first split
    /// transaction, every output unspent) and `T_base′` (every output spent).
    reuse: Vec<TxPlan>,
    /// Each setup block's coinbase coin, by height.
    funding: BTreeMap<u32, Coin>,
    mocktime: i64,
    height: u32,
    /// V's last post.
    post: Post,
}

fn err<T>(what: impl Into<String>) -> Result<T, String> {
    Err(what.into())
}

/// An RPC amount (DASH, 8 decimals) in satoshis: exact for every amount up to `MAX_MONEY`
/// (2.1e15 satoshis, inside an `f64`'s 53 bits).
pub fn amount(value: &Value) -> Result<i64, String> {
    let dash = value.as_f64().ok_or(format!("{value}: not an amount"))?;
    Ok((dash * 1e8).round() as i64)
}

/// The E-evo counters' change over one op.
type Delta = BTreeMap<&'static str, u64>;

/// Transactions by txid, each with a reason.
pub type Listed = Vec<(String, String)>;

/// The trial's own transactions: every one but a coinbase at `vtx[0]` (a block without one has
/// none to skip).
pub fn own_txids(plan: &BlockPlan, txids: &[Hash]) -> Vec<Hash> {
    let skip = usize::from(plan.txs.first().is_some_and(is_coinbase));
    txids.iter().skip(skip).copied().collect()
}

/// The rollback's mempool delta (Q-G, OD-L3-21): dashd gained exactly the disconnected own
/// transactions it re-admitted, and lost only what the submitted block took (`removed`: the
/// baseline mempool's removals for the block, with their reasons), each listed with its
/// reason. Anything else is a harness error.
pub fn mempool_delta(
    before: &BTreeSet<String>,
    after: &BTreeSet<String>,
    own: &[Hash],
    removed: &BTreeMap<Hash, Removal>,
) -> Result<Listed, String> {
    let disconnected: BTreeSet<String> = own.iter().map(hex).collect();
    let added: Vec<&String> = after.difference(before).collect();
    if let Some(stray) = added.iter().find(|t| !disconnected.contains(**t)) {
        return err(format!(
            "the rollback's mempool gained {stray}, not a rolled-back transaction"
        ));
    }
    let reasons: BTreeMap<String, &Removal> = removed.iter().map(|(id, r)| (hex(id), r)).collect();
    before
        .difference(after)
        .map(|lost| match reasons.get(lost) {
            Some(reason) => Ok((lost.clone(), reason.to_string())),
            None => err(format!("the rollback's mempool lost {lost}, which the block neither confirms nor conflicts with")),
        })
        .collect()
}

/// The E-evo hook's delta a body must give (M-3): `d.stx_hook` once per transaction, `w.stx`
/// once per transaction step 4 enters (all but a raw type-5 `vtx[0]`) for a body past step 4,
/// none before it, and between one and all inside it. A dead hook gives none for a body that
/// must count. A proposal counts in the `p.` mirrors and a submit or header op in the keys
/// themselves (OD-L3-43), so the other mode's pair must not move.
pub fn liveness(plan: &BlockPlan, reach: Reach, mode: Mode, delta: &Delta) -> Result<(), String> {
    let (own, other) = match mode {
        Mode::Proposal => (evo::PROPOSAL, ""),
        Mode::Submit | Mode::Header => ("", evo::PROPOSAL),
    };
    let get = |prefix: &str, key: &str| delta.get(format!("{prefix}{key}").as_str()).copied();
    let n = plan.txs.len() as u64;
    let skipped = u64::from(plan.txs.first().is_some_and(|t| t.tx_type == TRANSACTION_COINBASE));
    let (hook, wrapped) = (get(own, "d.stx_hook"), get(own, "w.stx"));
    let ok = match reach {
        Reach::None => hook == Some(0) && wrapped == Some(0),
        Reach::Full => hook == Some(n) && wrapped == Some(n - skipped),
        Reach::Partial => wrapped.is_some_and(|w| w >= 1 && w <= n - skipped) && hook >= wrapped,
    };
    if !ok {
        return err(format!(
            "§3.6 E-evo liveness: a {n}-transaction {mode:?} body with reach {reach:?} gave \
             {own}d.stx_hook {hook:?}, {own}w.stx {wrapped:?} (the E-evo hook is not live, or \
             counts what V did not run)"
        ));
    }
    let crossed = (get(other, "d.stx_hook"), get(other, "w.stx"));
    if crossed != (Some(0), Some(0)) {
        return err(format!(
            "§3.6 E-evo liveness: a {mode:?} body moved {other}d.stx_hook and {other}w.stx by \
             {crossed:?}, the other mode's keys (OD-L3-43)"
        ));
    }
    Ok(())
}

/// BL-564's wrapper edges per body (OD-L3-41's live witness, OD-L3-72 m3): `steps` says
/// whether the body entered ProcessSpecialTxsInBlock's step 6 and step 11 (an accepted body
/// both, a refused one where the inventory's [`Inventory::wrapper_steps`] puts its reject, a
/// header op neither), and `w.quorum` and `w.mnhf` move by one for a step entered and by none
/// otherwise, in the mode's keys (a proposal's `p.` mirrors, OD-L3-43). `None` is a reject in
/// that function's catch clauses, whose throw may come from any step: each own key moves by at
/// most one, and `w.mnhf` by no more than `w.quorum`, since step 11 runs only after step 6
/// (BL-672 N1). No op moves the other mode's keys.
pub fn wrappers(mode: Mode, steps: Option<(bool, bool)>, delta: &Delta) -> Result<(), String> {
    let (own, other) = match mode {
        Mode::Proposal => (evo::PROPOSAL, ""),
        Mode::Submit | Mode::Header => ("", evo::PROPOSAL),
    };
    let steps = if mode == Mode::Header {
        Some((false, false))
    } else {
        steps
    };
    let entered = [steps.map(|s| s.0), steps.map(|s| s.1)];
    for (key, entered) in ["w.quorum", "w.mnhf"].into_iter().zip(entered) {
        let want = entered.map(u64::from);
        let get = |prefix: &str| delta.get(format!("{prefix}{key}").as_str()).copied();
        if get(other) != Some(0) {
            return err(format!(
                "§3.6 E-evo: a {mode:?} op moved {other}{key} by {:?}, the other mode's key \
                 (OD-L3-43)",
                get(other)
            ));
        }
        if let Some(n) = want
            && get(own) != Some(n)
        {
            return err(format!(
                "§3.6 E-evo: a {mode:?} op (steps 6 and 11 entered {steps:?}) moved {own}{key} by \
                 {:?}, not {n} (V's site: {})",
                get(own),
                evo::site(&format!("g.{}", key.trim_start_matches("w.")))
            ));
        }
    }
    if steps.is_none() {
        let get = |key: &str| delta.get(format!("{own}{key}").as_str()).copied();
        let (quorum, mnhf) = (get("w.quorum"), get("w.mnhf"));
        if !matches!((quorum, mnhf), (Some(q), Some(m)) if m <= q && q <= 1) {
            return err(format!(
                "§3.6 E-evo: a {mode:?} op refused in the catch clauses moved {own}w.quorum by \
                 {quorum:?} and {own}w.mnhf by {mnhf:?}: each at most 1, w.mnhf no more than \
                 w.quorum (BL-672 N1)"
            ));
        }
    }
    Ok(())
}

/// The reject reason a reply carries: a submit's or proposal's string, or a header op's
/// `RPC_VERIFY_ERROR` message (rpc/mining.cpp:1102).
pub fn reject_reason(mode: Mode, reply: &Outcome) -> Option<&str> {
    match (mode, reply) {
        _ if rpc_answer(mode, reply) => None,
        (_, Ok(Value::String(reason))) => Some(reason),
        (Mode::Header, Err(e)) if e.code == Some(RPC_VERIFY_ERROR) => Some(&e.message),
        _ => None,
    }
}

/// `RPC_VERIFY_ERROR` (rpc/protocol.h).
const RPC_VERIFY_ERROR: i64 = -25;

/// What `submitblock` and a proposal answer from the block index or the tip before any
/// validation (rpc/mining.cpp:711-720, :1041-1062): no occurrence.
const RPC_ANSWERS: [&str; 5] = [
    "duplicate",
    "duplicate-invalid",
    "duplicate-inconclusive",
    "inconclusive",
    "inconclusive-not-best-prevblk",
];

/// Whether a block op's reply is one of `RPC_ANSWERS` (a header op's `duplicate` is
/// AcceptBlockHeader's reject, v:4625).
pub fn rpc_answer(mode: Mode, reply: &Outcome) -> bool {
    mode != Mode::Header
        && matches!(reply, Ok(Value::String(s)) if RPC_ANSWERS.contains(&s.as_str()))
}

/// The class of `hash`'s `BlockChecked` signal in V's step, if one fired.
fn checked_class(step: &Step, hash: &Hash) -> Option<String> {
    step.reply.checked.iter().find(|c| c.0.0 == *hash).and_then(|c| c.2.clone())
}

/// The variant's expectation against dashd's reply and its binding (the harness's
/// self-check): an accept or an RPC refusal binds nothing; a reject has the reason's prefix
/// and binds exactly the declared occurrence.
pub fn self_check(want: &Expect, reply: &Outcome, binding: &Option<Binding>) -> Result<(), String> {
    let ok = match (want, reply, binding) {
        (Expect::Accept, Ok(Value::Null), None) => true,
        (
            Expect::Reject {
                reason,
                occurrence,
            },
            Ok(Value::String(s)),
            Some(Binding::Bound(key)),
        ) => s.starts_with(reason) && key == occurrence,
        (
            Expect::Reject {
                reason,
                occurrence,
            },
            Err(e),
            Some(Binding::Bound(key)),
        ) => e.code == Some(RPC_VERIFY_ERROR) && e.message.starts_with(reason) && key == occurrence,
        (
            Expect::Refused {
                code,
                message,
            },
            Err(e),
            None,
        ) => e.code == Some(*code) && e.message == *message,
        (Expect::Answer(answer), Ok(Value::String(s)), None) => s == answer,
        _ => false,
    };
    if !ok {
        return err(format!(
            "operator drift: dashd replied {reply:?}, bound {binding:?}; the variant expects {want:?}"
        ));
    }
    Ok(())
}

/// `g.mnhf`'s cross-check beside V's count (§3.6, m-5, OD-L3-41): a body with an MNHF signal
/// is refused, with a unique binding strictly before ProcessSpecialTxsInBlock's step 11 (Core's
/// MNHF ProcessBlock), so the gated body never ran. An extraction error is recorded as such: Core's own extraction
/// fails it there too.
pub fn mnhf_gate(
    inventory: &Inventory,
    signals: &Result<usize, String>,
    accepted: bool,
    binding: &Option<Binding>,
) -> Result<(), String> {
    if !matches!(signals, Ok(n) if *n > 0) {
        return Ok(());
    }
    let before = match binding {
        Some(Binding::Bound(key)) => {
            inventory.row(key).is_some_and(|r| inventory.before_mnhf_step(r))
        }
        _ => false,
    };
    if accepted || !before {
        return err(format!(
            "§3.6 g.mnhf: a body with MNHF signals was not refused before step 11 (bound {binding:?}, \
             accepted {accepted}); V's site: {}",
            evo::site("g.mnhf")
        ));
    }
    Ok(())
}

/// A post before the first op: V down, nothing read.
fn down_post() -> Post {
    Post {
        down: true,
        flush_delta: Default::default(),
        tie: Vec::new(),
        persisted: None,
        tip: None,
        best_header: None,
        best_invalid: None,
        candidates: None,
        utxo: None,
        index: None,
        flushes: None,
        ibd: None,
        best_cl: None,
        sporks: None,
        tips: None,
        deployments: None,
    }
}

/// `g.mnhf`'s cross-check on V's side (OD-L3-18's mutant witness, plan §9): when V's verdict on a
/// submitted body with MNHF signals is not dashd's and dashd refused it, the run fails on
/// `g.mnhf`, naming V's site, ahead of the P1 divergence `error` that the same op raised. Where
/// dashd accepted it, `error` stands unchanged (OD-L3-26): V's site is not to blame, and
/// [`mnhf_gate`] fails a body either side accepts. dashd's refusal is any reply but null
/// (`submitblock` answers its reason as a result).
fn mnhf_v_side(signals: &Result<usize, String>, log: &LegLog, error: String) -> String {
    let Some(last) = log.ops.last() else {
        return error;
    };
    let dashd_refused = last.dashd != Ok(Value::Null);
    if !matches!(signals, Ok(n) if *n > 0) || last.v.reply.outcome == last.dashd || !dashd_refused {
        return error;
    }
    format!(
        "§3.6 g.mnhf: V's verdict {:?} on a body with MNHF signals is not dashd's refusal before \
         step 11 ({:?}); V's site: {}; P1: {error}",
        last.v.reply.outcome,
        last.dashd,
        evo::site("g.mnhf")
    )
}

/// §3.6's per-body gates, as the leg asserted them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BodyGates {
    /// `extract_signals`' count, or its error.
    pub mnhf_signals: Result<usize, String>,
    /// What the gates read: `getdeploymentinfo` at the parent on dashd and V's post there,
    /// and dashd's decode of the first transaction.
    pub dashd_parent: Value,
    pub v_parent: Value,
    pub first_decoded: Value,
}

impl BodyGates {
    pub fn json(&self) -> Value {
        let mnhf = match &self.mnhf_signals {
            Ok(n) => json!(n),
            Err(e) => json!(format!("extract_error({e})")),
        };
        json!({"dip0003": false, "v24": false, "cbtx": false, "mnhf_signals": mnhf})
    }

    /// A header op's record: the parent's states only, its body predicates n/a (§3.6).
    pub fn header_json(&self) -> Value {
        json!({"dip0003": false, "v24": false, "cbtx": "n/a", "mnhf_signals": "n/a"})
    }
}

impl<'a> Leg<'a> {
    /// dashd and V on the leg's arguments, after D8's check of V's parameters against Core's
    /// (before any chain session), and V's load checked (OD-L3-16: fail closed).
    fn open(config: &'a LegConfig<'a>, dir: &Path, log: &mut LegLog) -> Result<Leg<'a>, String> {
        let dir = private_dir(dir, &log.name)?;
        let v = NodeSide::with_argv(&log.chain_args, T0, &private_dir(&dir, "v")?, None)?;
        if let Some(message) = v.down() {
            log.load_refused = Some(message.to_owned());
            return err(format!("V's load was refused: {message}"));
        }
        evo::check(evo::EVO_KEYS, &config.inventory.evo_keys, &gates::KEYS)?;
        // The chain-parameter options only: `-par` is the node's.
        params::check_args(Pin::V24_0, &log.name, &CHAIN_ARGS, false, v.params())?;
        let datadir = private_dir(&dir, "dashd")?;
        let node = Process::start(&config.dashd.binary, &datadir, &log.chain_args, T0)?;
        log.dashd_argv = node.argv().to_vec();
        if log.dashd_argv.iter().any(|a| a.starts_with("-coinstatsindex")) {
            return err("P5 needs dashd without -coinstatsindex");
        }
        let cases = match config.fixture {
            Some(f) => vec![f.defect.case(config.par)],
            None => cases(config.set, config.par)?,
        };
        let (minted_for, mint_heights) = mints(&cases)?;
        let mut leg = Leg {
            config,
            node: Some(node),
            v,
            model: Model::new(build::genesis_hash()),
            ledger: Ledger::default(),
            mempool: Mempool::default(),
            txids: TxidLedger::default(),
            cases,
            coins: BTreeMap::new(),
            minted_for,
            mint_heights,
            reuse: Vec::new(),
            funding: BTreeMap::new(),
            mocktime: T0,
            height: 0,
            post: down_post(),
        };
        let chain = leg.rpc("getblockchaininfo", json!([]))?;
        if chain["chain"] != "regtest" || chain["blocks"] != 0 {
            return err(format!("dashd did not start at regtest genesis: {chain}"));
        }
        // The genesis post, for P6 at leg start.
        leg.step(ChainOp::SetTime(T0), None, Ok(Value::Null), log)?;
        leg.p6()?;
        Ok(leg)
    }

    fn node(&self) -> &Process {
        self.node.as_ref().expect("dashd runs")
    }

    fn rpc(&self, method: &str, params: Value) -> Result<Value, String> {
        planes::chain_only(method, &params)?;
        self.node().rpc().ok(method, params)
    }

    /// An RPC as the op's reply: its result, or its error in the shim's shape.
    fn reply(&self, method: &str, params: Value) -> Result<Outcome, String> {
        Ok(self.node().rpc().call(method, params)?.map_err(|e| RpcError {
            code: Some(e.code),
            message: e.message,
        }))
    }

    /// V's step for an op dashd ran (with `dashd`'s reply), logged, then P1; V down is a
    /// harness error. Also V's class for a proposal and the op's E-evo delta.
    fn step(
        &mut self,
        op: ChainOp,
        trial: Option<TrialId>,
        dashd: Outcome,
        log: &mut LegLog,
    ) -> Result<(Step, Option<&'static str>, Delta), String> {
        let s = self.v.step(&op).map_err(|u| format!("V refused {}: {u:?}", u.kind))?;
        if let Some(message) = self.v.down() {
            return err(format!("V went down: {message}"));
        }
        // §3.6: after every op, no gated body entered on V.
        let counts = self.v.evo_gates().ok_or("V's E-evo counters are not registered")?;
        evo::gate_check(&counts, log.ops.len())?;
        // A count that went back is a second registration hiding the live one (M-3).
        let delta: Delta = counts
            .iter()
            .map(|(k, n)| {
                let was = log.evo_counts.get(k).copied().unwrap_or_default();
                n.checked_sub(was).map(|d| (*k, d)).ok_or_else(|| {
                    format!(
                        "§3.6: V's E-evo counter {k} went from {was} to {n} at op {}",
                        log.ops.len()
                    )
                })
            })
            .collect::<Result<_, _>>()?;
        log.evo_counts = counts;
        self.post = s.step.post.clone();
        log.ops.push(Logged {
            op,
            trial,
            dashd,
            v: s.step.clone(),
            model_p7: None,
        });
        let last = log.ops.last().expect("an op ran");
        planes::p1(log.ops.len() - 1, &last.dashd, &last.v.reply.outcome)?;
        Ok((s.step, s.proposal_class, delta))
    }

    /// The setup chain (plan §3.2): per height the clock, the cases there, then the setup block.
    fn chain(&mut self, log: &mut LegLog) -> Result<(), String> {
        if let Some(fixture) = self.config.fixture {
            return self.fixture(fixture, log);
        }
        let last = self.cases.iter().map(|c| c.height).max().unwrap_or(0) + 1;
        for h in 1..=last {
            self.mocktime = T0 + 150 * i64::from(h);
            self.rpc("setmocktime", json!([self.mocktime]))?;
            self.step(ChainOp::SetTime(self.mocktime), None, Ok(Value::Null), log)?;
            for i in 0..self.cases.len() {
                let case = self.cases[i];
                if case.height != h || !self.selected(&case) {
                    continue;
                }
                match scenario_steps(case.id.op, case.id.variant) {
                    Some(steps) => self.scenario(&case, i, steps, log)?,
                    None => {
                        for kind in case.kinds(self.config.set) {
                            self.trial(&case, i, kind, log)?;
                        }
                    }
                }
            }
            self.setup_block(h, None, log)?;
        }
        Ok(())
    }

    /// dashd's median time past at the block `hash`.
    fn mediantime(&self, hash: &str) -> Result<u32, String> {
        self.rpc("getblockheader", json!([hash]))?["mediantime"]
            .as_u64()
            .and_then(|t| u32::try_from(t).ok())
            .ok_or_else(|| format!("getblockheader {hash}: no mediantime"))
    }

    fn selected(&self, case: &Case) -> bool {
        self.config.only.is_none_or(|only| only == case.id.op || only == case.id.to_string())
    }

    /// The setup transactions at `h`: at [`SPLIT_HEIGHT`] the split (each case's three coins
    /// under its lock, [`SPLIT_CASES`] cases per transaction, each spending the next coinbase from
    /// height 1), at the height after it `T_base′` (spending the next coinbase), spent whole at
    /// the one after that.
    fn setup_txs(&self, h: u32) -> Result<Vec<TxPlan>, String> {
        let coinbase = |k: u32| self.funding.get(&k).copied().ok_or(format!("no coinbase at {k}"));
        let spend = |coin: Coin, outputs: Vec<(i64, Vec<u8>)>| {
            TxPlan::simple(vec![build::Input::new(coin.outpoint, coin.lock.script_sig())], outputs)
        };
        let chunks = self.cases.len().div_ceil(SPLIT_CASES) as u32;
        let mut txs = Vec::new();
        if h == SPLIT_HEIGHT {
            for (chunk, k) in self.cases.chunks(SPLIT_CASES).zip(1..) {
                let coin = coinbase(k)?;
                let each = (coin.value - SETUP_FEE) / (3 * chunk.len() as i64);
                let outputs = chunk
                    .iter()
                    .flat_map(|c| std::iter::repeat_n((each, case_lock(c).script_pubkey()), 3))
                    .collect();
                txs.push(spend(coin, outputs));
            }
        } else if h == SPLIT_HEIGHT + 1 {
            let coin = coinbase(chunks + 1)?;
            txs.push(spend(coin, vec![(coin.value - SETUP_FEE, Lock::True.script_pubkey())]));
        } else if h == SPLIT_HEIGHT + 2 {
            let base = self.reuse.get(1).ok_or("no T_base′")?;
            let coin = Coin {
                outpoint: (base.txid(), 0),
                value: base.outputs[0].0,
                lock: Lock::True,
                height: h - 1,
                coinbase: false,
            };
            txs.push(spend(coin, vec![(coin.value - SETUP_FEE, Lock::True.script_pubkey())]));
        }
        Ok(txs)
    }

    /// The setup block at `h`: `generateblock raw(51) [<its setup txs>] true` on dashd, or at a
    /// mint height the coinbase-only template paying `MAX_MONEY`, submitted; its bytes to V. An
    /// AV-on fixture submits the `recorded` bytes instead (plan §7.4's replay of P).
    fn setup_block(
        &mut self,
        h: u32,
        recorded: Option<&[u8]>,
        log: &mut LegLog,
    ) -> Result<(), String> {
        let prev = self.model.tip();
        let lock = Lock::True;
        let setup = self.setup_txs(h)?;
        let descriptor = format!("raw({})", to_hex(&lock.script_pubkey()));
        let (hash, bytes) = if let Some(bytes) = recorded {
            let reply = self.reply("submitblock", json!([to_hex(bytes)]))?;
            if reply != Ok(Value::Null) {
                return err(format!("recorded setup block {h}: dashd replied {reply:?}"));
            }
            let head: [u8; 80] =
                bytes.get(..80).and_then(|b| b.try_into().ok()).ok_or("no header")?;
            (header_hash(&head), bytes.to_vec())
        } else if self.mint_heights.contains(&h) {
            let template = self.rpc("generateblock", json!([descriptor, [], false]))?;
            let mut plan = BlockPlan::from_block(&from_hex(
                template["hex"].as_str().ok_or("no template hex")?,
            )?)?;
            plan.txs[0].outputs[0].0 = MAX_MONEY;
            let built = plan.build();
            let reply = self.reply("submitblock", json!([to_hex(&built.bytes)]))?;
            if reply != Ok(Value::Null) || !setup.is_empty() {
                return err(format!("mint {h}: dashd replied {reply:?}"));
            }
            (built.hash, built.bytes)
        } else {
            let raw: Vec<String> = setup
                .iter()
                .map(|tx| to_hex(&dash_consensus::primitives::core_serialize(&*tx.tx())))
                .collect();
            let made = self.rpc("generateblock", json!([descriptor, raw, true]))?;
            let hash = rpc_hash(&made["hash"])?;
            let hex_block = self.rpc("getblock", json!([hex(&hash), 0]))?;
            (hash, from_hex(hex_block.as_str().ok_or("getblock: not hex")?)?)
        };
        let plan = BlockPlan::from_block(&bytes)?;
        let txids: Vec<Hash> = plan.txs.iter().map(|t| t.txid()).collect();
        let head: [u8; 80] = bytes[..80].try_into().expect("a header");
        if header_hash(&head) != hash
            || plan.prev != prev
            || !is_coinbase(&plan.txs[0])
            || plan.txs[1..] != setup[..]
        {
            return err(format!("setup block {h}: not a coinbase and its setup txs on the tip"));
        }
        self.txids.record(Owner::Setup(h), &txids[1..], &[])?;
        self.body_gates(&prev, &plan)?;
        let (_, _, delta) = self.step(ChainOp::Block(bytes), None, Ok(Value::Null), log)?;
        liveness(&plan, Reach::Full, Mode::Submit, &delta)
            .and_then(|()| wrappers(Mode::Submit, Some((true, true)), &delta))
            .map_err(|e| format!("setup block {h}: {e}"))?;
        self.model.connect(hash, prev);
        self.ledger.connect(&plan, &txids, h)?;
        self.mempool.remove_for_block(&plan, &txids);
        self.height = h;
        let cb = &plan.txs[0];
        if cb.outputs.len() != 1 || cb.outputs[0].1 != lock.script_pubkey() {
            return err(format!("setup block {h}: its coinbase is not one {lock:?} output"));
        }
        if h == SPLIT_HEIGHT {
            for (c, tx) in setup.iter().enumerate() {
                let txid = tx.txid();
                for (j, out) in tx.outputs.chunks(3).enumerate() {
                    let i = c * SPLIT_CASES + j;
                    let coins = (0..3)
                        .map(|n| Coin {
                            outpoint: (txid, (3 * j + n) as u32),
                            value: out[n].0,
                            lock: case_lock(&self.cases[i]),
                            height: h,
                            coinbase: false,
                        })
                        .collect();
                    self.coins.insert(i, coins);
                }
            }
        }
        if h == SPLIT_HEIGHT || h == SPLIT_HEIGHT + 1 {
            self.reuse.push(setup[0].clone());
        }
        self.funding.insert(
            h,
            Coin {
                outpoint: (txids[0], 0),
                value: cb.outputs[0].0,
                lock,
                height: h,
                coinbase: true,
            },
        );
        self.planes(log)?;
        let pool = self.rpc("getrawmempool", json!([]))?;
        if mempool_set(&pool)? != self.mempool.txids().iter().map(hex).collect() {
            return err(format!(
                "after setup block {h}: dashd's mempool {pool} is not the model's"
            ));
        }
        if BOUNDARIES.iter().any(|b| h + 1 == *b || h == *b) || h.is_multiple_of(144) {
            self.p6()?;
        }
        Ok(())
    }

    /// One trial (plan §3.2): its block from a fresh template, its manifest, the op on both
    /// nodes, the planes, its binding, and a positive submit's rollback.
    fn trial(&mut self, case: &Case, i: usize, kind: Kind, log: &mut LegLog) -> Result<(), String> {
        let h = self.height + 1;
        let prev = self.model.tip();
        let trial = TrialId {
            case: case.id,
            kind,
        };
        let prepared = self.prepare(case, i, trial)?;
        let built = prepared.plan.build();
        let own = own_txids(&prepared.plan, &built.txids);
        // Declared reuse (plan §3.2): a REUSE_SETUP case repeats T_base or T_base′.
        let reused: Vec<Hash> = if REUSE_SETUP.contains(&case.op.name) {
            self.reuse.iter().map(TxPlan::txid).collect()
        } else {
            Vec::new()
        };
        self.txids.record(Owner::Trial(trial.to_string()), &own, &reused)?;
        self.manifest(&trial, h, &built.hash, &prepared.coins)?;
        self.absences(&trial, &prepared.spent, REUSE_SETUP.contains(&case.op.name))?;
        if kind == Kind::Seq {
            // Warm on purpose: its own proposal first, credited as the sequence case only.
            let (reply, _, delta) = self.propose(&built.bytes, trial, log)?;
            if reply != Ok(Value::Null) {
                return err(format!(
                    "{trial}: the sequence case's proposal was refused: {reply:?}"
                ));
            }
            liveness(&prepared.plan, Reach::Full, Mode::Proposal, &delta)
                .and_then(|()| wrappers(Mode::Proposal, Some((true, true)), &delta))
                .map_err(|e| format!("{trial}: {e}"))?;
        }
        let op = Op {
            trial,
            plan: &prepared.plan,
            built: &built,
            mode: kind.mode(),
            want: case.expect(kind.mode()),
            height: h,
            prev,
            mtp: prepared.mtp,
        };
        if let Some(pending) = self.execute(&op, log)? {
            self.rollback(pending, log)?;
        }
        Ok(())
    }

    /// The case's block on the tip, from a fresh template: its operator's mutation with
    /// `trial`'s id, and the coins the manifest checks.
    fn prepare(&mut self, case: &Case, i: usize, trial: TrialId) -> Result<Prepared, String> {
        let h = self.height + 1;
        let prev = self.model.tip();
        let template = self.rpc("generateblock", json!(["raw(51)", [], false]))?;
        let mut plan =
            BlockPlan::from_block(&from_hex(template["hex"].as_str().ok_or("no template hex")?)?)?;
        if plan.txs.len() != 1 || plan.prev != prev {
            return err(format!("{trial}: the template is not one coinbase on the tip"));
        }
        if i64::from(plan.time) != self.mocktime {
            return err(format!("{trial}: the template's time is not the clock"));
        }
        let mtp = self.mediantime(&hex(&prev))?;
        if h != case.height || !case.id.site.holds(h) {
            return err(format!("{trial}: at height {h}, not its site's"));
        }
        let coins = match self.minted_for.get(&i) {
            Some(mints) => mints
                .iter()
                .map(|&m| self.funding.get(&self.mint_heights[m]).copied())
                .collect::<Option<Vec<_>>>()
                .ok_or(format!("{trial}: a mint is missing"))?,
            None => self.coins.get(&i).cloned().ok_or(format!("{trial}: no funding"))?,
        };
        let immature = *self.funding.get(&(h - 99)).ok_or("no immature coin")?;
        let mature = *self.funding.get(&(h - build::MATURITY)).ok_or("no mature coin")?;
        let spent = self.funding.get(&1).ok_or("no coinbase 1")?.outpoint;
        let reuse = self.reuse.clone();
        // BIP68's time base for coin 0: the MTP of its height's parent (tx_verify.cpp:80).
        let coin_parent = coins.first().map_or(0, |c| c.height).saturating_sub(1);
        let coin_mtp = match self.rpc("getblockhash", json!([coin_parent]))? {
            Value::String(hash) => self.mediantime(&hash)?,
            other => return err(format!("getblockhash {coin_parent}: {other}")),
        };
        let sb_limit = match case.id.site {
            Site::SB(_) => amount(&self.rpc("getsuperblockbudget", json!([h]))?)?,
            _ => 0,
        };
        let l3 = L3Cx {
            trial: trial.bytes(),
            carrier: case.variant.carrier,
            variant: case.variant.name,
            site: case.id.site,
            reward: plan.txs[0].outputs.iter().map(|o| o.0).sum(),
            sb_limit,
            spent,
            mature,
            coin_mtp,
            reuse: &reuse,
        };
        let cx = Cx {
            height: h,
            mtp,
            mocktime: self.mocktime,
            coins: &coins,
            immature: Some(immature),
            l3: Some(&l3),
        };
        (case.op.apply)(&mut plan, &cx);
        if plan.txs.first().is_some_and(is_coinbase) && !EXACT_COINBASE.contains(&case.op.name) {
            let bare = build::tx_size(&plan.txs[0]);
            plan.txs[0].inputs[0].script_sig.extend(push(&trial.bytes()));
            if build::tx_size(&plan.txs[0]) != bare + COINBASE_TAG {
                return err(format!("{trial}: the coinbase tag is not {COINBASE_TAG} bytes"));
            }
        }
        let mut all_coins = coins;
        all_coins.extend([immature, mature]);
        Ok(Prepared {
            plan,
            mtp,
            coins: all_coins,
            spent,
        })
    }

    /// One op of a trial or scenario step on both nodes: its planes, its binding against its
    /// expectation, and the model after it. An accepted submit's rollback is returned, for the
    /// caller to run.
    fn execute(&mut self, s: &Op<'_>, log: &mut LegLog) -> Result<Option<Pending>, String> {
        let (trial, plan, built, mode) = (s.trial, s.plan, s.built, s.mode);
        let prev = s.prev;
        let measured = ledger::measure(plan, &built.txids, s.height, s.mtp, &self.ledger);
        // S5's parent is in no index: the body's gates are read at the tip, where it would run.
        let gate_parent = if self.model.knows(&prev) {
            prev
        } else {
            self.model.tip()
        };
        let gates = self.body_gates(&gate_parent, plan)?;
        let parent = self.parent_state(&prev)?;
        let baseline = (self.ledger.clone(), self.mempool.clone(), self.mempool_now()?);
        let before = self.effects(&built.hash, Some(&built.bytes))?;
        let head = built.header;
        let (reply, v_class, delta) = match mode {
            Mode::Proposal => self.propose(&built.bytes, trial, log)?,
            Mode::Submit => {
                let dashd = self.reply("submitblock", json!([to_hex(&built.bytes)]))?;
                let op = ChainOp::Block(built.bytes.clone());
                let (step, _, delta) = self
                    .step(op, Some(trial), dashd.clone(), log)
                    .map_err(|e| mnhf_v_side(&gates.mnhf_signals, log, e))?;
                (dashd, checked_class(&step, &built.hash), delta)
            }
            Mode::Header => {
                let dashd = self.reply("submitheader", json!([to_hex(&head)]))?;
                let (step, _, delta) =
                    self.step(ChainOp::Header(head), Some(trial), dashd.clone(), log)?;
                // OD-L3-24: Core raises no BlockChecked for a header op, so V's is a divergence.
                if !step.reply.checked.is_empty() {
                    return err(format!(
                        "{trial}: V raised BlockChecked for a header op: {:?}",
                        step.reply.checked
                    ));
                }
                (dashd, None, delta)
            }
        };
        let op = log.ops.len() - 1;
        let effects = self.effects(&built.hash, Some(&built.bytes))?;
        let accepted = reply == Ok(Value::Null);
        let answered = rpc_answer(mode, &reply);
        if (mode == Mode::Proposal || answered) && effects != before {
            return err(format!("{trial}: a {mode:?} changed the index: {before:?} → {effects:?}"));
        }
        match mode {
            _ if answered => {}
            Mode::Submit => self.observe_submit(plan, built, &prev, effects, accepted, s.height)?,
            Mode::Header => {
                self.observe_header(&trial, &built.hash, &prev, before, effects, accepted)?
            }
            Mode::Proposal => {}
        }
        let facts = Facts {
            network: self.rpc("getblockchaininfo", json!([]))?["chain"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            // §3.6's gates hold for every body the leg submits (`body_gates` above).
            dip3: false,
            cbtx: false,
            mnsync_synced: is_synced(&self.rpc("mnsync", json!(["status"]))?)?,
            par: trial.case.par,
            parent,
            measured,
        };
        let inventory = self.config.inventory;
        let binding = reject_reason(mode, &reply).map(|reason| {
            inventory.bind(reason, mode, &facts, (mode != Mode::Proposal).then_some(effects))
        });
        self_check(&s.want, &reply, &binding).map_err(|e| format!("{trial}: {e}"))?;
        let bound_row = match &binding {
            Some(Binding::Bound(key)) => Some(inventory.row(key).expect("a bound row")),
            _ => None,
        };
        // v:4684-4699: a header refused for an unmarked parent's failed ancestor marked that
        // parent's branch BLOCK_FAILED_CHILD, which both sides must show.
        if bound_row.is_some_and(|row| row.guards.get("parent") == Some(&json!(WALKED)))
            && (!self.model.failed_child_walk(&prev) || !self.effects(&prev, None)?.failed)
        {
            return err(format!("{trial}: the walk left the parent {} unmarked", hex(&prev)));
        }
        let bound_class = bound_row.map(|row| inventory.class(row));
        // M-3: the body's E-evo delta, from where it was refused.
        // A header op runs no body (§3.6: its body predicates are n/a).
        let reach = match (mode, accepted, bound_row) {
            (Mode::Header, _, _) => Reach::None,
            (_, true, _) => Reach::Full,
            (_, false, Some(row)) => inventory.reach(row)?,
            (_, false, None) => Reach::None,
        };
        let steps = match (accepted, bound_row) {
            (true, _) => Some((true, true)),
            (false, Some(row)) => inventory.wrapper_steps(row)?,
            (false, None) => Some((false, false)),
        };
        liveness(plan, reach, mode, &delta)
            .and_then(|()| wrappers(mode, steps, &delta))
            .map_err(|e| format!("{trial}: {e}"))?;
        if mode != Mode::Header {
            mnhf_gate(inventory, &gates.mnhf_signals, accepted, &binding)
                .map_err(|e| format!("{trial}: {e}"))?;
        }
        let view = self.planes(log)?;
        let mut evo = if mode == Mode::Header {
            gates.header_json()
        } else {
            gates.json()
        };
        let own = if mode == Mode::Proposal {
            evo::PROPOSAL
        } else {
            ""
        };
        let seam = |key: &str| {
            let key = format!("{own}{key}");
            let n = delta.get(key.as_str()).copied();
            (key, json!(n))
        };
        evo["delta"] = Value::Object([seam("d.stx_hook"), seam("w.stx")].into_iter().collect());
        self.record(log, &trial, trial.kind.name(), op, &built.hash, &prev, &view, &gates)?;
        log.trials.push(TrialRecord {
            trial,
            hash: built.hash,
            op,
            mode,
            reply,
            binding,
            bound_class,
            v_class,
            effects,
            evo,
            declined: Vec::new(),
            removed: Vec::new(),
            gained: Vec::new(),
        });
        Ok((accepted && mode == Mode::Submit).then(|| Pending {
            trial,
            record: log.trials.len() - 1,
            plan: plan.clone(),
            hash: built.hash,
            txids: built.txids.clone(),
            own: own_txids(plan, &built.txids),
            prev,
            gates,
            baseline,
        }))
    }

    /// A named scenario's run (plan §4 group P): its first block on the tip as its operator
    /// builds it, the others from that one, each step a trial with its record. The state each
    /// step relies on is declared and checked on both sides before it runs.
    fn scenario(
        &mut self,
        case: &Case,
        i: usize,
        steps: &'static [ScenarioStep],
        log: &mut LegLog,
    ) -> Result<(), String> {
        let id = |k: usize| TrialId {
            case: case.id,
            kind: Kind::Step(steps[k].0, steps[k].1),
        };
        let h = self.height + 1;
        let tip = self.model.tip();
        let prepared = self.prepare(case, i, id(0))?;
        let first = prepared.plan.build();
        let own = own_txids(&prepared.plan, &first.txids);
        self.txids.record(Owner::Trial(case.id.to_string()), &own, &[])?;
        self.manifest(&id(0), h, &first.hash, &prepared.coins)?;
        let op = |k: usize, plan, built, height, prev, mtp| Op {
            trial: id(k),
            plan,
            built,
            mode: steps[k].1,
            want: steps[k].2,
            height,
            prev,
            mtp,
        };
        let on_tip = |k: usize| op(k, &prepared.plan, &first, h, tip, prepared.mtp);
        let fail = |what: String| err(format!("{}: {what}", case.id));
        match case.id.op {
            "s1_mutated_then_honest" => {
                // Declared reuse: the honest block drops the duplicate, so it has the mutated
                // block's header and transactions (CVE-2012-2459's form).
                let mut honest_plan = prepared.plan.clone();
                honest_plan.txs.pop();
                let honest = honest_plan.build();
                let distinct: BTreeSet<Hash> = first.txids.iter().copied().collect();
                if honest.hash != first.hash
                    || honest.txids.iter().copied().collect::<BTreeSet<_>>() != distinct
                {
                    return fail(
                        "the honest block is not the mutated block's header and txids".into(),
                    );
                }
                self.execute(&on_tip(0), log)?;
                self.declare_unknown(&id(1), &honest.hash)?;
                let honest_op = op(1, &honest_plan, &honest, h, tip, prepared.mtp);
                if let Some(pending) = self.execute(&honest_op, log)? {
                    self.rollback(pending, log)?;
                }
            }
            "s2_child_of_invalid" => {
                self.execute(&on_tip(0), log)?;
                let child_plan = child(&prepared.plan, first.hash, h + 1, id(1));
                let child = child_plan.build();
                self.declare_parent(&id(1), &first.hash, ParentState::Failed)?;
                self.declare_unknown(&id(1), &child.hash)?;
                let mtp = self.mediantime(&hex(&first.hash))?;
                self.execute(&op(1, &child_plan, &child, h + 1, first.hash, mtp), log)?;
            }
            "s3_resubmit" | "s3h_failed_header_resubmit" => {
                let pending = self.execute(&on_tip(0), log)?;
                // Declared reuse: the same block (header) again, as the first step left it.
                let want = Effects {
                    known: true,
                    failed: pending.is_none(),
                    stored: true,
                };
                if self.effects(&first.hash, Some(&first.bytes))? != want {
                    return fail(format!("before the resubmit, the block is not {want:?}"));
                }
                self.execute(&on_tip(1), log)?;
                if let Some(pending) = pending {
                    self.rollback(pending, log)?;
                }
            }
            "s4_proposal_off_tip" => {
                let up = self.model.parent_of(&tip).ok_or("the tip has no parent")?;
                let plan = child(&prepared.plan, up, h - 1, id(0));
                let block = plan.build();
                self.declare_unknown(&id(0), &block.hash)?;
                let mtp = self.mediantime(&hex(&up))?;
                self.execute(&op(0, &plan, &block, h - 1, up, mtp), log)?;
            }
            "s5_unknown_parent" => {
                let mut nowhere = [0x5e; 32];
                nowhere[..8].copy_from_slice(&id(0).bytes());
                let plan = child(&prepared.plan, nowhere, h, id(0));
                let block = plan.build();
                self.declare_unknown(&id(0), &nowhere)?;
                self.declare_unknown(&id(0), &block.hash)?;
                self.execute(&op(0, &plan, &block, h, nowhere, prepared.mtp), log)?;
            }
            "s6_child_of_unmarked_descendant" => {
                let c_plan = child(&prepared.plan, first.hash, h + 1, id(1));
                let c = c_plan.build();
                let d_plan = child(&c_plan, c.hash, h + 2, id(3));
                let d = d_plan.build();
                self.execute(&on_tip(0), log)?;
                let mtp = self.mediantime(&hex(&first.hash))?;
                self.execute(&op(1, &c_plan, &c, h + 1, first.hash, mtp), log)?;
                self.execute(&on_tip(2), log)?;
                // What separates v:4699 from v:4651: C′ unmarked and headers-only on both
                // sides, descending from F′ in the failed set.
                let headers_only = Effects {
                    known: true,
                    failed: false,
                    stored: false,
                };
                if self.effects(&c.hash, None)? != headers_only
                    || self.tip_status(&c.hash)? != "headers-only"
                {
                    return fail("before D′, C′ is not unmarked and headers-only".into());
                }
                self.declare_parent(&id(3), &c.hash, ParentState::UnmarkedFailedAncestor)?;
                self.declare_unknown(&id(3), &d.hash)?;
                let mtp = self.mediantime(&hex(&c.hash))?;
                self.execute(&op(3, &d_plan, &d, h + 2, c.hash, mtp), log)?;
                if self.tip_status(&c.hash)? != "invalid" {
                    return fail("after D′, C′'s tip is not invalid".into());
                }
            }
            other => return fail(format!("no runner for the scenario {other}")),
        }
        Ok(())
    }

    /// A scenario's declared absence: `hash` in neither side's index nor the model.
    fn declare_unknown(&self, trial: &TrialId, hash: &Hash) -> Result<(), String> {
        if self.dashd_known_stored(hash)?.0
            || self.v_entry(hash)?.is_some()
            || self.model.knows(hash)
        {
            return err(format!("{trial}: declared unknown, {} is indexed", hex(hash)));
        }
        Ok(())
    }

    /// A scenario's declared parent state, read on both sides (`parent_state`).
    fn declare_parent(
        &self,
        trial: &TrialId,
        prev: &Hash,
        want: ParentState,
    ) -> Result<(), String> {
        let state = self.parent_state(prev)?;
        if state != want {
            return err(format!("{trial}: the parent {} is {state:?}, not {want:?}", hex(prev)));
        }
        Ok(())
    }

    /// dashd's `getchaintips` status for the tip `hash`.
    fn tip_status(&self, hash: &Hash) -> Result<String, String> {
        let tips = self.rpc("getchaintips", json!([]))?;
        planes::tip_row(&tips, hash)
            .and_then(|row| row["status"].as_str().map(str::to_owned))
            .ok_or_else(|| format!("{} is no chain tip", hex(hash)))
    }

    fn propose(
        &mut self,
        bytes: &[u8],
        trial: TrialId,
        log: &mut LegLog,
    ) -> Result<(Outcome, Option<String>, Delta), String> {
        let dashd =
            self.reply("getblocktemplate", json!([{"mode": "proposal", "data": to_hex(bytes)}]))?;
        let (_, class, delta) =
            self.step(ChainOp::Proposal(bytes.to_vec()), Some(trial), dashd.clone(), log)?;
        Ok((dashd, class.map(str::to_owned), delta))
    }

    /// The model after a submit, from dashd's effects (rev1 §3.3's path table), which V's must
    /// equal.
    fn observe_submit(
        &mut self,
        plan: &BlockPlan,
        built: &Built,
        prev: &Hash,
        effects: Effects,
        accepted: bool,
        h: u32,
    ) -> Result<(), String> {
        let hash = built.hash;
        match (effects.known, effects.stored, effects.failed) {
            (false, false, false) => {}
            (true, true, false) if accepted => {
                self.model.connect(hash, *prev);
                self.ledger.connect(plan, &built.txids, h)?;
                self.mempool.remove_for_block(plan, &built.txids);
            }
            (true, true, failed) => self.model.connect_reject(hash, *prev, !failed),
            (true, false, failed) => self.model.contextual_reject(hash, *prev, !failed),
            other => return err(format!("submit effects outside the path table: {other:?}")),
        }
        Ok(())
    }

    /// The model after a header op, from dashd's effects: an accepted header is indexed
    /// headers-only; a refused one leaves the header's own entry as it was.
    fn observe_header(
        &mut self,
        trial: &TrialId,
        hash: &Hash,
        prev: &Hash,
        before: Effects,
        effects: Effects,
        accepted: bool,
    ) -> Result<(), String> {
        match (accepted, effects.known, effects.stored, effects.failed) {
            (true, true, false, false) => self.model.header(*hash, *prev),
            (false, ..) if effects == before => {}
            _ => {
                return err(format!(
                    "{trial}: header effects outside the path table: {before:?} → {effects:?} \
                     (accepted {accepted})"
                ));
            }
        }
        Ok(())
    }

    /// A positive submit's rollback (plan §3.3 P7): `invalidateblock`, the planes, the coins back
    /// at the baseline, and dashd's mempool delta (Q-G, OD-L3-21): it gained the block's own
    /// transactions only, and lost only what the block took, each listed with its reason, on
    /// the submit's record.
    fn rollback(&mut self, p: Pending, log: &mut LegLog) -> Result<(), String> {
        let (trial, hash, (ledger, mempool, before)) = (p.trial, p.hash, p.baseline);
        let dashd = self.reply("invalidateblock", json!([hex(&hash)]))?;
        self.step(ChainOp::Invalidate(hash), Some(trial), dashd, log)?;
        let op = log.ops.len() - 1;
        self.model.invalidate_tip(hash);
        self.ledger = ledger;
        let after = self.mempool_now()?;
        let taken = mempool.removed_for_block(&p.plan, &p.txids);
        let removed =
            mempool_delta(&before, &after, &p.own, &taken).map_err(|e| format!("{trial}: {e}"))?;
        let gained = after.difference(&before).cloned().collect();
        self.mempool = mempool;
        for (id, _) in &removed {
            self.mempool.txs.retain(|t, _| hex(t) != *id);
        }
        let mut declined = Vec::new();
        for (tx, id) in
            p.plan.txs.iter().map(|t| (t, t.txid())).filter(|(_, id)| p.own.contains(id))
        {
            if after.contains(&hex(&id)) {
                self.mempool.txs.insert(id, tx.clone());
            } else {
                let raw = to_hex(&dash_consensus::core_serialize(&*tx.tx()));
                let r = self.rpc("testmempoolaccept", json!([[raw]]))?;
                let reason = r[0]["reject-reason"].as_str().unwrap_or("accepted").to_owned();
                declined.push((hex(&id), reason));
            }
        }
        if self.mempool.txids().iter().map(hex).collect::<BTreeSet<_>>() != after {
            return err(format!("{trial}: after the rollback, dashd's mempool is not the model's"));
        }
        let effects = self.effects(&hash, None)?;
        if effects
            != (Effects {
                known: true,
                failed: true,
                stored: true,
            })
        {
            return err(format!("{trial}: rollback effects {effects:?}"));
        }
        let view = self.planes(log)?;
        self.record(log, &trial, "rollback", op, &hash, &p.prev, &view, &p.gates)?;
        let record = &mut log.trials[p.record];
        (record.declined, record.removed, record.gained) = (declined, removed, gained);
        Ok(())
    }

    fn mempool_now(&self) -> Result<BTreeSet<String>, String> {
        mempool_set(&self.rpc("getrawmempool", json!([]))?)
    }

    /// The per-trial manifest (plan §3.2), before the trial: a mismatch is a harness error.
    fn manifest(
        &mut self,
        trial: &TrialId,
        h: u32,
        hash: &Hash,
        coins: &[Coin],
    ) -> Result<(), String> {
        let prev = self.model.tip();
        let fail = |what: &str| err(format!("{trial}: manifest: {what}"));
        if rpc_hash(&self.rpc("getbestblockhash", json!([]))?)? != prev
            || self.post.tip != Some(block_at(h - 1, prev))
        {
            return fail("the tip is not the parent on both sides");
        }
        if self.rpc("getdeploymentinfo", json!([hex(&prev)]))?
            != self.post.deployments.clone().unwrap_or_default()
        {
            return fail("the parent's deployments differ");
        }
        match self.node().rpc().call("getblockheader", json!([hex(hash)]))? {
            Err(e) if e.code == UNKNOWN_BLOCK => {}
            other => return fail(&format!("the trial's block is known to dashd: {other:?}")),
        }
        if self.v_entry(hash)?.is_some() || self.model.knows(hash) {
            return fail("the trial's block is known to V");
        }
        let v_coins = self.v_outpoints()?;
        for c in coins {
            let out = self.rpc("gettxout", json!([hex(&c.outpoint.0), c.outpoint.1, false]))?;
            if out.is_null()
                || !v_coins.contains(&c.outpoint)
                || !self.ledger.coins.contains_key(&c.outpoint)
            {
                return fail(&format!(
                    "funding {}:{} is not unspent chain-only",
                    hex(&c.outpoint.0),
                    c.outpoint.1
                ));
            }
        }
        planes::mnsync_unsynced(&self.rpc("mnsync", json!(["status"]))?).or_else(|e| fail(&e))?;
        let ibd = self.rpc("getblockchaininfo", json!([]))?["initialblockdownload"].as_bool();
        if ibd != Some(false) || self.post.ibd != Some(false) {
            return fail("a side is in IBD");
        }
        Ok(())
    }

    /// The declared absences and reuse (plan §3.2), chain-only on both sides: the coin setup spent
    /// at the split (`spent_earlier`'s) is gone, and for a `REUSE_SETUP` case `T_base`'s outputs
    /// are all unspent and `T_base′`'s all spent (`bip30`'s and its control's).
    fn absences(&self, trial: &TrialId, spent: &(Hash, u32), reuse: bool) -> Result<(), String> {
        let v_coins = self.v_outpoints()?;
        let unspent = |o: &(Hash, u32)| -> Result<bool, String> {
            let dashd = !self.rpc("gettxout", json!([hex(&o.0), o.1, false]))?.is_null();
            if dashd != v_coins.contains(o) {
                return err(format!("{trial}: {}:{} is unspent on one side only", hex(&o.0), o.1));
            }
            Ok(dashd)
        };
        fn outputs(tx: &TxPlan) -> Vec<(Hash, u32)> {
            (0..tx.outputs.len() as u32).map(|n| (tx.txid(), n)).collect()
        }
        let mut ok = !unspent(spent)?;
        if let (true, [base, spent_base]) = (reuse, &self.reuse[..]) {
            for o in outputs(base) {
                ok &= unspent(&o)?;
            }
            for o in outputs(spent_base) {
                ok &= !unspent(&o)?;
            }
        }
        if !ok {
            return err(format!("{trial}: manifest: a declared absence or reuse does not hold"));
        }
        Ok(())
    }

    /// The parent's state for the binder, read on both sides and held to the model's (m-4).
    fn parent_state(&self, prev: &Hash) -> Result<ParentState, String> {
        if !self.model.knows(prev) {
            return match self.dashd_known_stored(prev)?.0 || self.v_entry(prev)?.is_some() {
                false => Ok(ParentState::Unknown),
                true => err(format!("m-4: the parent {} is indexed, not modelled", hex(prev))),
            };
        }
        let entries = self.v.index().ok_or("V's index is not readable")?;
        let status = entries
            .iter()
            .find(|e| e.hash.as_bytes() == prev)
            .map(|e| e.status)
            .ok_or_else(|| format!("m-4: the parent {} is not in V's index", hex(prev)))?;
        let failed_ancestor = entries.iter().any(|e| {
            e.status & planes::FAILED_VALID != 0 && self.model.is_ancestor(e.hash.as_bytes(), prev)
        });
        let header = self.rpc("getblockheader", json!([hex(prev)]))?;
        let tips = self.rpc("getchaintips", json!([]))?;
        planes::parent_state(
            prev,
            self.model.parent_state(prev),
            planes::v_parent_state(status, failed_ancestor),
            planes::dashd_parent_failed(prev, &header, &tips)?,
        )
    }

    /// V's index entry for `hash`: its status.
    fn v_entry(&self, hash: &Hash) -> Result<Option<u32>, String> {
        let entries = self.v.index().ok_or("V's index is not readable")?;
        Ok(entries.iter().find(|e| e.hash.as_bytes() == hash).map(|e| e.status))
    }

    fn v_outpoints(&self) -> Result<BTreeSet<(Hash, u32)>, String> {
        let coins = self
            .v
            .tip_coins()
            .ok_or("V serves no coins")?
            .map_err(|e| format!("V's coins: {e:?}"))?;
        Ok(coins
            .iter()
            .map(|(k, _)| {
                let o = k.outpoint();
                (*o.hash.as_bytes(), o.index)
            })
            .collect())
    }

    /// What dashd shows of `hash`: whether `getblockheader` knows it and `getblock` serves it.
    fn dashd_known_stored(&self, hash: &Hash) -> Result<(bool, bool), String> {
        let rpc = self.node().rpc();
        let known = match rpc.call("getblockheader", json!([hex(hash)]))? {
            Ok(_) => true,
            Err(e) if e.code == UNKNOWN_BLOCK => false,
            Err(e) => return err(format!("getblockheader: {e:?}")),
        };
        let stored = known && rpc.call("getblock", json!([hex(hash), 0]))?.is_ok();
        Ok((known, stored))
    }

    /// P2: `hash`'s effects on dashd (`getblockheader`, `getchaintips`, `getblock <h> 0`), which
    /// V's index must show too. A known block that is no tip (S6's F′ under its headers-only
    /// child) has no `getchaintips` row: whether it failed is read from a proposal of its
    /// `bytes`, which dashd answers from the block's index entry alone and changes nothing
    /// (rpc/mining.cpp:711-716).
    fn effects(&self, hash: &Hash, bytes: Option<&[u8]>) -> Result<Effects, String> {
        let (known, stored) = self.dashd_known_stored(hash)?;
        let tips = if known {
            self.rpc("getchaintips", json!([]))?
        } else {
            Value::Null
        };
        let dashd = match bytes {
            Some(bytes) if known && planes::tip_row(&tips, hash).is_none() => {
                let answer = self.rpc(
                    "getblocktemplate",
                    json!([{"mode": "proposal", "data": to_hex(bytes)}]),
                )?;
                Effects {
                    known,
                    failed: planes::failed_by_answer(&answer)?,
                    stored,
                }
            }
            _ => planes::dashd_effects(known, stored, &tips, hash)?,
        };
        planes::p2(hash, dashd, self.v_entry(hash)?)?;
        Ok(dashd)
    }

    /// What P3 and P5 read from dashd.
    fn view(&self) -> Result<View, String> {
        Ok(View {
            tips: self.rpc("getchaintips", json!([]))?,
            best: self.rpc("getbestblockhash", json!([]))?,
            info: self.rpc("getblockchaininfo", json!([]))?,
            txoutset: self.rpc("gettxoutsetinfo", json!(["muhash"]))?,
        })
    }

    fn v_muhash(&self) -> Result<String, String> {
        let coins = self
            .v
            .tip_coins()
            .ok_or("V serves no coins")?
            .map_err(|e| format!("V's coins: {e:?}"))?;
        Ok(hex(&muhash_refs(coins.iter().map(|(k, c)| (k.outpoint(), c.as_ref())))))
    }

    /// P3, P5 and P7 after an op (the model updated): their reads, decided by [`planes`].
    fn planes(&mut self, log: &mut LegLog) -> Result<View, String> {
        let at = log.ops.len() - 1;
        let view = self.view()?;
        let p7 = self.model.p7();
        planes::p3(at, &view, &self.post, p7.best_header.0)?;
        planes::p5(&view, &self.model.tip(), &self.v_muhash()?, &hex(&self.ledger.muhash()))?;
        planes::p7(&self.post, p7.best_header)?;
        log.ops.last_mut().expect("an op ran").model_p7 = Some(p7);
        Ok(view)
    }

    /// P6: `getdeploymentinfo` at the tip vs V's post.
    fn p6(&self) -> Result<(), String> {
        planes::p6(self.height, &self.rpc("getdeploymentinfo", json!([]))?, &self.post)
    }

    /// When the run records: what the planes read after the trial's op `op`, for the
    /// witnesses' fixtures (the replay adds Core's post at `op`).
    #[allow(clippy::too_many_arguments)]
    fn record(
        &self,
        log: &mut LegLog,
        trial: &TrialId,
        what: &str,
        op: usize,
        hash: &Hash,
        parent: &Hash,
        view: &View,
        gates: &BodyGates,
    ) -> Result<(), String> {
        if !self.config.record {
            return Ok(());
        }
        let (known, stored) = self.dashd_known_stored(hash)?;
        let entries = self.v.index().ok_or("V's index is not readable")?;
        let (dashd_reply, v_reply) = (
            planes::outcome_json(&log.ops[op].dashd),
            planes::outcome_json(&log.ops[op].v.reply.outcome),
        );
        let p7 = self.model.p7();
        let pair = |(h, hash): &(u32, Hash)| json!([h, hex(hash)]);
        log.recorded.push(json!({
            "trial": trial.to_string(),
            "what": what,
            "op": op,
            "hash": hex(hash),
            "parent": hex(parent),
            "dashd": {
                "reply": dashd_reply,
                "known": known,
                "stored": stored,
                "view": view.json(),
                "getdeploymentinfo": self.rpc("getdeploymentinfo", json!([]))?,
                "getdeploymentinfo_parent": gates.dashd_parent,
                "first_decoded": gates.first_decoded,
                "mnsync": self.rpc("mnsync", json!(["status"]))?,
            },
            "v": {
                "reply": v_reply,
                "post": planes::post_planes(&self.post),
                "status": self.v_entry(hash)?,
                "muhash": self.v_muhash()?,
                "index": entries.iter().map(|e| json!([to_hex(e.hash.as_bytes()), e.height, e.status, e.has_txs])).collect::<Vec<_>>(),
                "index_digest": to_hex(&index_digest(&entries).0),
                "deployments_parent": gates.v_parent,
                "evo_counts": log.evo_counts,
            },
            "ledger_muhash": hex(&self.ledger.muhash()),
            "model_best_header": pair(&p7.best_header),
            "model_tip": hex(&self.model.tip()),
            "model_p7": {
                "best_invalid": p7.best_invalid.as_ref().map(pair),
                "candidates": p7.candidates.iter().map(pair).collect::<Vec<_>>(),
            },
        }));
        Ok(())
    }

    /// §3.6's per-body gates for a block on `parent`: DIP3 and V24 inactive for it on both
    /// sides, no decoded CbTx, and the MNHF signals `extract_signals` finds (an extraction error
    /// recorded as such, never as none). A failed gate is a harness error.
    fn body_gates(&self, parent: &Hash, plan: &BlockPlan) -> Result<BodyGates, String> {
        let dashd = self.rpc("getdeploymentinfo", json!([hex(parent)]))?;
        let v = self.post.deployments.clone().unwrap_or_default();
        // An empty body (`empty_block`) has no first transaction to decode, so no CbTx.
        let first_decoded = match plan.txs.first() {
            Some(tx) => {
                let first = to_hex(&dash_consensus::core_serialize(&*tx.tx()));
                self.rpc("decoderawtransaction", json!([first]))?
            }
            None => Value::Null,
        };
        planes::body_gates(parent, &dashd, &v, &first_decoded)?;
        let built = plan.build();
        let mut cursor = &built.bytes[..];
        let block = dash_consensus::decode_block(&mut cursor)
            .map_err(|e| format!("§3.6: the body does not decode: {e:?}"))?
            .into_value();
        Ok(BodyGates {
            mnhf_signals: dash_dml::mnhf::extract_signals(&block)
                .map(|bits| bits.len())
                .map_err(|e| e.reject_reason().to_string()),
            dashd_parent: dashd,
            v_parent: v,
            first_decoded,
        })
    }
}

fn mempool_set(pool: &Value) -> Result<BTreeSet<String>, String> {
    pool.as_array()
        .ok_or_else(|| format!("getrawmempool: {pool}"))?
        .iter()
        .map(|t| t.as_str().map(str::to_owned).ok_or_else(|| format!("getrawmempool: {t}")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use dash_oracle_sys::Reply;

    /// OD-L3-26: a body with a signal that dashd refused and V accepted fails on `g.mnhf`,
    /// naming V's site, with P1's text kept; one dashd accepted and V refused keeps P1's error
    /// alone, as does a body without a signal or an op with matching verdicts.
    #[test]
    fn mnhf_v_side_blames_v_only_when_dashd_refused() {
        let log = |dashd: Outcome, v: Outcome| LegLog {
            name: "mnhf-signal".into(),
            set: Set::Full,
            par: Par::One,
            chain_args: Vec::new(),
            dashd_argv: Vec::new(),
            ops: vec![Logged {
                op: ChainOp::SetTime(T0),
                trial: None,
                dashd,
                v: Step {
                    reply: Reply {
                        checked: Vec::new(),
                        outcome: v,
                    },
                    records: Vec::new(),
                    events: Vec::new(),
                    post: down_post(),
                },
                model_p7: None,
            }],
            trials: Vec::new(),
            load_refused: None,
            evo_counts: BTreeMap::new(),
            recorded: Vec::new(),
            fixture: None,
            av: None,
            seconds: 0.0,
            error: None,
        };
        let refused = Ok(json!("bad-tx-type-dip3-inactive"));
        let p1 = || "P1 diverges at op 0".to_owned();
        let v_accepted = mnhf_v_side(&Ok(1), &log(refused.clone(), Ok(Value::Null)), p1());
        assert!(v_accepted.starts_with("§3.6 g.mnhf: V's verdict Ok(Null)"), "{v_accepted}");
        assert!(v_accepted.contains(evo::site("g.mnhf")), "{v_accepted}");
        assert!(v_accepted.ends_with(&format!("P1: {}", p1())), "{v_accepted}");
        let thrown = Err(RpcError {
            code: Some(-1),
            message: "refused".into(),
        });
        assert!(mnhf_v_side(&Ok(1), &log(thrown, Ok(Value::Null)), p1()).contains("g.mnhf"));
        assert_eq!(mnhf_v_side(&Ok(1), &log(Ok(Value::Null), refused.clone()), p1()), p1());
        assert_eq!(mnhf_v_side(&Ok(0), &log(refused.clone(), Ok(Value::Null)), p1()), p1());
        assert_eq!(
            mnhf_v_side(&Err("x".into()), &log(refused.clone(), Ok(Value::Null)), p1()),
            p1()
        );
        assert_eq!(mnhf_v_side(&Ok(1), &log(refused.clone(), refused), p1()), p1());
    }
    use crate::chain::build::{Input, TxPlan, coinbase};

    /// A block of `txs`, and their txids.
    fn plan_of(txs: Vec<TxPlan>) -> (BlockPlan, Vec<Hash>) {
        let plan = BlockPlan {
            prev: [0; 32],
            version: 4,
            time: 0,
            bits: 0,
            txs,
            merkle_override: None,
        };
        let txids = plan.txs.iter().map(TxPlan::txid).collect();
        (plan, txids)
    }

    /// The legs' arguments set the activation heights group B's sites and the ledger's facts
    /// are on.
    #[test]
    fn the_chain_args_set_the_activation_heights() {
        for (name, height) in [
            ("dip0001", DIP0001_HEIGHT),
            ("dersig", DERSIG_HEIGHT),
            ("cltv", CLTV_HEIGHT),
            ("csv", CSV_HEIGHT),
            ("bip147", BIP147_HEIGHT),
        ] {
            let arg = format!("-testactivationheight={name}@{height}");
            assert!(CHAIN_ARGS.contains(&arg.as_str()), "{arg}");
        }
    }

    /// tier 1 has every variant that is not full-only once per tier-1 site per leg, with ids as
    /// plan §3.2 writes them; its positive cases (the control, m-8, among them) have a T-seq.
    #[test]
    fn tier1_has_the_catalogue_at_reg() {
        let par1 = cases(Set::Tier1, Par::One).expect("tier 1's cases");
        let tier1 = |v: &Variant| {
            if v.full_only {
                0
            } else {
                v.sites.iter().filter(|s| s.tier1()).count()
            }
        };
        assert_eq!(
            par1.len(),
            catalogue().map(|o| o.variants.iter().map(tier1).sum::<usize>()).sum::<usize>()
        );
        assert_eq!(par1[0].id.to_string(), "cb_amount.reg@Reg/par1");
        let positive: Vec<String> =
            par1.iter().filter(|c| c.positive()).map(|c| c.id.to_string()).collect();
        assert_eq!(
            positive[..3],
            ["control.reg@Reg/par1", "cb_value.bound-1@Reg/par1", "cb_value.bound@Reg/par1"]
        );
        assert!(positive.contains(&"tx_size_100k.100001@P1/par1".to_owned()));
        assert!(!positive.contains(&"tx_size_100k.100001@Reg/par1".to_owned()));
        // Positive cases add a T-seq; a proposal-only row has its proposal alone; header mode
        // is the full set's.
        for c in &par1 {
            let kinds = match c.op.name {
                "empty_block" => vec![Kind::Prop],
                _ if c.positive() => vec![Kind::Cold, Kind::Prop, Kind::Seq],
                _ => vec![Kind::Cold, Kind::Prop],
            };
            assert_eq!(c.kinds(Set::Tier1), kinds, "{}", c.id);
        }
        let ids: BTreeSet<String> = par1.iter().map(|c| c.id.to_string()).collect();
        assert_eq!(ids.len(), par1.len(), "unique case ids");
        let reg: Vec<u32> =
            par1.iter().filter(|c| c.id.site == Site::Reg).map(|c| c.height).collect();
        assert_eq!(reg, (CASE_BASE..CASE_BASE + reg.len() as u32).collect::<Vec<_>>());
        let p1: Vec<u32> =
            par1.iter().filter(|c| c.id.site == Site::P1).map(|c| c.height).collect();
        assert_eq!(p1, (P1_BASE..P1_BASE + p1.len() as u32).collect::<Vec<_>>());
        assert!(par1.iter().all(|c| !c.variant.full_only));
        assert!(BOUNDARIES.iter().all(|b| *b < CASE_BASE));
        assert_eq!(chain_args(Par::Four).last().map(String::as_str), Some("-par=4"));
    }

    /// The full set adds the header-mode trials and the named scenarios (plan §3.4), each
    /// scenario run at Reg with its steps as its trials; the scenarios come last, so no other
    /// case's height or funding moves.
    #[test]
    fn the_full_set_adds_header_trials_and_scenarios() {
        let full = cases(Set::Full, Par::Four).expect("the full set's cases");
        let header: Vec<&str> = full
            .iter()
            .filter(|c| c.kinds(Set::Full).contains(&Kind::Header))
            .map(|c| c.id.op)
            .collect();
        assert_eq!(header, ["bits_plus", "time_at_mtp", "time_too_new", "version_3"]);
        let runs: Vec<(String, Vec<&str>)> = full
            .iter()
            .filter(|c| scenario_steps(c.id.op, c.id.variant).is_some())
            .map(|c| (c.id.to_string(), c.kinds(Set::Full).iter().map(|k| k.name()).collect()))
            .collect();
        let want = [
            ("s1_mutated_then_honest.reg", vec!["mutated", "honest"]),
            ("s2_child_of_invalid.reg", vec!["parent", "child"]),
            ("s3_resubmit.failed", vec!["first", "again"]),
            ("s3_resubmit.valid", vec!["first", "again"]),
            ("s3h_failed_header_resubmit.reg", vec!["block", "header"]),
            ("s4_proposal_off_tip.reg", vec!["prop"]),
            ("s5_unknown_parent.reg", vec!["submit"]),
            (
                "s6_child_of_unmarked_descendant.header",
                vec!["f_header", "c_header", "f_block", "d_header"],
            ),
            (
                "s6_child_of_unmarked_descendant.submit",
                vec!["f_header", "c_header", "f_block", "d_block"],
            ),
        ];
        let scenarios = want.len();
        assert_eq!(runs, want.map(|(id, steps)| (format!("{id}@Reg/par4"), steps)).to_vec());
        let reg: Vec<&Case> = full.iter().filter(|c| c.id.site == Site::Reg).collect();
        let first = reg.iter().position(|c| scenario_steps(c.id.op, c.id.variant).is_some());
        assert_eq!(first, Some(reg.len() - scenarios), "the scenarios are Reg's last cases");
        assert!(
            cases(Set::Tier1, Par::Four).expect("tier 1's cases").iter().all(|c| scenario_steps(
                c.id.op,
                c.id.variant
            )
            .is_none())
        );
    }

    /// A child block: the template's header fields on another parent, its coinbase the
    /// trial's own at the height.
    #[test]
    fn a_child_is_the_template_on_another_parent() {
        let template = BlockPlan {
            prev: [1; 32],
            version: 0x2000_0000,
            time: T0 as u32,
            bits: 0x207f_ffff,
            txs: vec![build::coinbase(299, 5, 9)],
            merkle_override: Some([2; 32]),
        };
        let trial = TrialId {
            case: CaseId {
                op: "s2_child_of_invalid",
                variant: "reg",
                site: Site::Reg,
                par: Par::One,
            },
            kind: Kind::Step("child", Mode::Submit),
        };
        let c = child(&template, [7; 32], 300, trial);
        assert_eq!(
            (c.prev, c.version, c.time, c.bits),
            ([7; 32], template.version, template.time, template.bits)
        );
        assert_eq!(c.txs, [build::coinbase(300, 0, u64::from_le_bytes(trial.bytes()))]);
        assert_eq!(c.merkle_override, None);
        assert_eq!(trial.to_string(), "s2_child_of_invalid.reg@Reg/par1+child");
    }

    /// n-6: one self-check of reply and binding against the variant.
    #[test]
    fn the_self_check_reads_replies_and_bindings_as_expectations() {
        let key = "validation.cpp:ConnectBlock#5:bad-txns-nonfinal";
        let reject = Expect::Reject {
            reason: "bad-txns-nonfinal",
            occurrence: key,
        };
        let bound = Some(Binding::Bound(key.into()));
        assert_eq!(self_check(&reject, &Ok(Value::from("bad-txns-nonfinal")), &bound), Ok(()));
        assert!(self_check(&reject, &Ok(Value::Null), &None).is_err());
        let other =
            Some(Binding::Bound("validation.cpp:ContextualCheckBlock#2:bad-txns-nonfinal".into()));
        assert!(self_check(&reject, &Ok(Value::from("bad-txns-nonfinal")), &other).is_err());
        let ambiguous = Some(Binding::Ambiguous(vec![key.into()]));
        assert!(self_check(&reject, &Ok(Value::from("bad-txns-nonfinal")), &ambiguous).is_err());
        let refused = Expect::Refused {
            code: -22,
            message: "Block does not start with a coinbase",
        };
        let e = RpcError {
            code: Some(-22),
            message: "Block does not start with a coinbase".into(),
        };
        assert_eq!(self_check(&refused, &Err(e.clone()), &None), Ok(()));
        // Group P's RPC answers bind nothing; a header's `duplicate` is a reject (v:4625).
        let duplicate = Ok(Value::from("duplicate"));
        assert_eq!(reject_reason(Mode::Submit, &duplicate), None);
        assert_eq!(self_check(&Expect::Answer("duplicate"), &duplicate, &None), Ok(()));
        assert!(self_check(&Expect::Answer("duplicate-invalid"), &duplicate, &None).is_err());
        let cached = Err(RpcError {
            code: Some(RPC_VERIFY_ERROR),
            message: "duplicate".into(),
        });
        assert_eq!(reject_reason(Mode::Header, &cached), Some("duplicate"));
        assert!(
            self_check(
                &refused,
                &Err(RpcError {
                    code: Some(-25),
                    ..e
                }),
                &None
            )
            .is_err()
        );
        assert_eq!(self_check(&Expect::Accept, &Ok(Value::Null), &None), Ok(()));
        assert!(self_check(&Expect::Accept, &Ok(Value::from("bad-cb-amount")), &None).is_err());
        // A header op's reject is its RPC_VERIFY_ERROR message (rpc/mining.cpp:1102).
        let header_key = "validation.cpp:ContextualCheckBlockHeader#5:time-too-new";
        let header_reject = Expect::Reject {
            reason: "time-too-new",
            occurrence: header_key,
        };
        let verify = Err(RpcError {
            code: Some(-25),
            message: "time-too-new".into(),
        });
        assert_eq!(reject_reason(Mode::Header, &verify), Some("time-too-new"));
        assert_eq!(reject_reason(Mode::Submit, &verify), None);
        assert_eq!(reject_reason(Mode::Header, &Ok(Value::Null)), None);
        let header_bound = Some(Binding::Bound(header_key.into()));
        assert_eq!(self_check(&header_reject, &verify, &header_bound), Ok(()));
        let deser = Err(RpcError {
            code: Some(-22),
            message: "time-too-new".into(),
        });
        assert!(self_check(&header_reject, &deser, &header_bound).is_err());
    }

    fn spend(prevout: (Hash, u32)) -> TxPlan {
        TxPlan::simple(
            vec![Input::new(prevout, Vec::new())],
            vec![(1_000, Lock::True.script_pubkey())],
        )
    }

    /// m-8: the trial's own transactions skip only a coinbase at `vtx[0]`.
    #[test]
    fn own_transactions_skip_only_the_coinbase() {
        let a = spend(([1; 32], 0));
        let b = spend(([2; 32], 0));
        let (with, ids) = plan_of(vec![coinbase(5, 1, 1), a.clone(), b.clone()]);
        assert_eq!(own_txids(&with, &ids), vec![a.txid(), b.txid()]);
        let (without, ids) = plan_of(vec![a.clone(), b.clone()]);
        assert_eq!(own_txids(&without, &ids), vec![a.txid(), b.txid()], "no coinbase to skip");
    }

    /// m-8, OD-L3-21: the rollback's delta gains only rolled-back transactions and loses only
    /// what the block took, each with its reason.
    #[test]
    fn the_rollback_mempool_delta() {
        let cold = spend(([1; 32], 0));
        let child = spend((cold.txid(), 0));
        let other = spend(([3; 32], 0));
        let mut seq = spend(([1; 32], 0));
        seq.outputs[0].0 = 999;
        let mut pool = Mempool::default();
        for tx in [&cold, &child, &other] {
            pool.txs.insert(tx.txid(), tx.clone());
        }
        let (plan, ids) = plan_of(vec![coinbase(5, 1, 1), seq.clone()]);
        let own = own_txids(&plan, &ids);
        let taken = pool.removed_for_block(&plan, &ids);
        let set = |txs: &[&TxPlan]| txs.iter().map(|t| hex(&t.txid())).collect::<BTreeSet<_>>();
        let before = set(&[&cold, &child, &other]);
        // Baseline: T-seq's own transaction back, T-cold's conflict and its child gone.
        let removed =
            mempool_delta(&before, &set(&[&seq, &other]), &own, &taken).expect("the rule");
        assert_eq!(
            removed,
            vec![
                (hex(&cold.txid()), format!("conflict on {}:0", hex(&[1; 32]))),
                (hex(&child.txid()), format!("descendant of {}", hex(&cold.txid()))),
            ]
            .into_iter()
            .collect::<BTreeMap<_, _>>()
            .into_iter()
            .collect::<Vec<_>>()
        );
        // A gained transaction that was not rolled back.
        let stray = spend(([4; 32], 0));
        let e =
            mempool_delta(&before, &set(&[&seq, &other, &stray]), &own, &taken).expect_err("stray");
        assert!(e.contains("not a rolled-back"), "{e}");
        // A lost transaction the block did not take.
        let e = mempool_delta(&before, &set(&[&seq]), &own, &taken).expect_err("lost");
        assert!(e.contains(&hex(&other.txid())) && e.contains("neither confirms"), "{e}");
        // Nothing gained, nothing lost (ATMP declined the disconnected one).
        assert!(mempool_delta(&set(&[&other]), &set(&[&other]), &own, &BTreeMap::new()).is_ok());
    }

    /// A submit's delta: every counter V has, the seam's pair as given.
    fn delta(hook: u64, wrapped: u64) -> Delta {
        let mut d: Delta = gates::KEYS.iter().map(|k| (*k, 0)).collect();
        d.insert("d.stx_hook", hook);
        d.insert("w.stx", wrapped);
        d
    }

    /// The same delta counted as a proposal's, in the `p.` mirrors (OD-L3-43).
    fn proposed(d: Delta) -> Delta {
        let mirror = |k: &str| gates::KEYS.iter().copied().find(|m| *m == format!("p.{k}"));
        let mut p: Delta = gates::KEYS.iter().map(|k| (*k, 0)).collect();
        for (k, n) in d.into_iter().filter(|(k, _)| !k.starts_with("p.")) {
            p.insert(mirror(k).expect("a mirror"), n);
        }
        p
    }

    /// M-3: the E-evo hook's per-body delta, and a dead hook failing.
    #[test]
    fn the_e_evo_hook_is_live_per_body() {
        let (setup, _) = plan_of(vec![coinbase(5, 1, 1)]);
        let live =
            |plan: &BlockPlan, reach: Reach, d: Delta| liveness(plan, reach, Mode::Submit, &d);
        assert_eq!(live(&setup, Reach::Full, delta(1, 1)), Ok(()));
        let dead = live(&setup, Reach::Full, delta(0, 0)).expect_err("a dead hook");
        assert!(dead.contains("not live"), "{dead}");
        let (three, _) = plan_of(vec![coinbase(5, 1, 1), spend(([1; 32], 0)), spend(([2; 32], 0))]);
        assert_eq!(live(&three, Reach::Full, delta(3, 3)), Ok(()), "N + 1");
        assert!(live(&three, Reach::Full, delta(3, 2)).is_err());
        assert_eq!(live(&three, Reach::None, delta(0, 0)), Ok(()));
        assert!(live(&three, Reach::None, delta(3, 3)).is_err(), "counted a body never run");
        assert_eq!(live(&three, Reach::Partial, delta(2, 2)), Ok(()));
        assert!(live(&three, Reach::Partial, delta(0, 0)).is_err());
        // A raw type-5 coinbase is hooked, not wrapped (m-8's D2/D3 shape).
        let mut cb = coinbase(5, 1, 1);
        cb.tx_type = TRANSACTION_COINBASE;
        let (raw5, _) = plan_of(vec![cb, spend(([1; 32], 0))]);
        assert_eq!(live(&raw5, Reach::Full, delta(2, 1)), Ok(()));
        assert!(live(&raw5, Reach::Full, delta(2, 2)).is_err());
    }

    /// OD-L3-43: a proposal's seam counts are its `p.` mirrors, a submit's never.
    #[test]
    fn a_proposal_is_live_in_its_mirrors() {
        let (three, _) = plan_of(vec![coinbase(5, 1, 1), spend(([1; 32], 0)), spend(([2; 32], 0))]);
        let p = |d: Delta| liveness(&three, Reach::Full, Mode::Proposal, &d);
        assert_eq!(p(proposed(delta(3, 3))), Ok(()));
        let e = p(delta(3, 3)).expect_err("counted as a submit");
        assert!(e.contains("p.d.stx_hook Some(0), p.w.stx Some(0)"), "{e}");
        let e = liveness(&three, Reach::Full, Mode::Submit, &proposed(delta(3, 3)))
            .expect_err("counted as a proposal");
        assert!(e.contains("not live"), "{e}");
        // Both pairs moving fails each mode on the other's keys.
        let mut both = proposed(delta(3, 3));
        both.extend([("d.stx_hook", 3), ("w.stx", 3)]);
        let e = p(both.clone()).expect_err("both");
        assert!(e.contains("moved d.stx_hook and w.stx by (Some(3), Some(3))"), "{e}");
        let e = liveness(&three, Reach::Full, Mode::Submit, &both).expect_err("both");
        assert!(e.contains("moved p.d.stx_hook and p.w.stx"), "{e}");
        // A header op counts nothing in either.
        let header = |d: Delta| liveness(&three, Reach::None, Mode::Header, &d);
        assert_eq!(header(delta(0, 0)), Ok(()));
        assert!(header(proposed(delta(1, 1))).is_err());
    }

    /// OD-L3-41: an accepted body moves each wrapper once in its mode's keys, a header op none.
    #[test]
    fn an_accepted_body_moves_each_wrapper_once() {
        let with = |quorum: u64, mnhf: u64| {
            let mut d = delta(0, 0);
            d.extend([("w.quorum", quorum), ("w.mnhf", mnhf)]);
            d
        };
        let both = Some((true, true));
        assert_eq!(wrappers(Mode::Submit, both, &with(1, 1)), Ok(()));
        assert_eq!(wrappers(Mode::Proposal, both, &proposed(with(1, 1))), Ok(()));
        // A dropped increment (the OD-L3-41 mutant's shape) fails, in either mode, naming the
        // key's own site.
        let e = wrappers(Mode::Submit, both, &with(1, 0)).expect_err("w.mnhf dropped");
        assert!(e.contains("moved w.mnhf by Some(0), not 1"), "{e}");
        assert!(e.contains(evo::site("g.mnhf")), "{e}");
        let e = wrappers(Mode::Proposal, both, &proposed(with(0, 1))).expect_err("dropped");
        assert!(e.contains("moved p.w.quorum by Some(0), not 1"), "{e}");
        // Counted in the other mode's keys fails.
        let e = wrappers(Mode::Proposal, both, &with(1, 1)).expect_err("as a submit");
        assert!(e.contains("moved w.quorum by Some(1), the other mode's key"), "{e}");
        // A header op moves nothing, whatever it is given.
        assert_eq!(wrappers(Mode::Header, both, &with(0, 0)), Ok(()));
        assert!(wrappers(Mode::Header, both, &with(1, 0)).is_err());
    }

    /// OD-L3-72 n4: a site's next case height is found inside its window, and a search that
    /// reaches the window's end fails rather than running on.
    #[test]
    fn a_case_height_past_its_window_fails() {
        use crate::chain::mutate::{BUDGET_WINDOW, SUPERBLOCK_START};
        assert_eq!(next_height(Site::Reg, CASE_BASE), Ok(CASE_BASE));
        assert_eq!(next_height(Site::BO, BUDGET_START), Ok(BUDGET_START + BUDGET_WINDOW));
        for (site, from) in
            [(Site::Reg, BUDGET_START), (Site::P1, DIP0001_HEIGHT), (Site::BW, SUPERBLOCK_START)]
        {
            let e = next_height(site, from).expect_err("past the window");
            assert!(e.contains("before its window's end"), "{e}");
        }
        assert!(next_height(Site::At(5), 6).is_err(), "a pinned height already passed");
    }

    /// OD-L3-72 m3: a refused body moves each wrapper by the steps it entered, in either mode:
    /// both (refused below ConnectBlock's ProcessSpecialTxsInBlock call), step 6 only (between
    /// steps 6 and 11), neither (before step 6); a catch-all reject asserts neither key's move.
    #[test]
    fn a_refused_body_moves_each_wrapper_by_the_steps_it_entered() {
        let with = |quorum: u64, mnhf: u64| {
            let mut d = delta(0, 0);
            d.extend([("w.quorum", quorum), ("w.mnhf", mnhf)]);
            d
        };
        for mode in [Mode::Submit, Mode::Proposal] {
            let d = |q, m| {
                if mode == Mode::Proposal {
                    proposed(with(q, m))
                } else {
                    with(q, m)
                }
            };
            let own = if mode == Mode::Proposal {
                evo::PROPOSAL
            } else {
                ""
            };
            // Entered both: a dropped bump on the refused path fails.
            assert_eq!(wrappers(mode, Some((true, true)), &d(1, 1)), Ok(()));
            let e = wrappers(mode, Some((true, true)), &d(1, 0)).expect_err("w.mnhf dropped");
            assert!(e.contains(&format!("moved {own}w.mnhf by Some(0), not 1")), "{e}");
            assert!(e.contains(evo::site("g.mnhf")) && !e.contains(evo::site("g.quorum")), "{e}");
            let e = wrappers(mode, Some((true, true)), &d(0, 1)).expect_err("w.quorum dropped");
            assert!(e.contains(&format!("moved {own}w.quorum by Some(0), not 1")), "{e}");
            assert!(e.contains(evo::site("g.quorum")) && !e.contains(evo::site("g.mnhf")), "{e}");
            // Step 6 only.
            assert_eq!(wrappers(mode, Some((true, false)), &d(1, 0)), Ok(()));
            let e = wrappers(mode, Some((true, false)), &d(1, 1)).expect_err("step 11 counted");
            assert!(e.contains(&format!("moved {own}w.mnhf by Some(1), not 0")), "{e}");
            // Neither: a bump for a step the body never entered fails.
            assert_eq!(wrappers(mode, Some((false, false)), &d(0, 0)), Ok(()));
            let e = wrappers(mode, Some((false, false)), &d(1, 0)).expect_err("step 6 counted");
            assert!(e.contains(&format!("moved {own}w.quorum by Some(1), not 0")), "{e}");
            // The catch clauses: each own key at most 1, w.mnhf no more than w.quorum.
            for (q, m) in [(0, 0), (1, 0), (1, 1)] {
                assert_eq!(wrappers(mode, None, &d(q, m)), Ok(()), "({q}, {m})");
            }
            for (q, m) in [(0, 1), (2, 0), (2, 2)] {
                let e = wrappers(mode, None, &d(q, m)).expect_err("outside the catch bounds");
                assert!(e.contains("refused in the catch clauses"), "({q}, {m}): {e}");
            }
        }
        let crossed = wrappers(Mode::Proposal, None, &with(1, 1)).expect_err("the other mode's");
        assert!(crossed.contains("the other mode's key"), "{crossed}");
    }

    /// m-5: the plan's `g.mnhf` rule, with a10-5's OD-L3-18 baseline (a pre-DIP3 body with a
    /// signal, refused at st:228) passing.
    #[test]
    fn a_body_with_a_signal_is_refused_before_step_11() {
        let inv = Inventory::load().expect("the fixture");
        let bound = |key: &str| Some(Binding::Bound(key.into()));
        let st228 = bound("specialtxman.cpp:CheckSpecialTxInner#1:bad-tx-type-dip3-inactive");
        assert_eq!(mnhf_gate(&inv, &Ok(1), false, &st228), Ok(()), "the OD-L3-18 baseline");
        assert_eq!(mnhf_gate(&inv, &Ok(0), true, &None), Ok(()), "no signal");
        assert_eq!(mnhf_gate(&inv, &Err("bad".into()), false, &None), Ok(()), "recorded as such");
        assert!(mnhf_gate(&inv, &Ok(1), true, &None).is_err(), "accepted");
        let late = bound("validation.cpp:ConnectBlock#9:bad-cb-amount");
        let e = mnhf_gate(&inv, &Ok(1), false, &late).expect_err("after step 11");
        assert!(e.contains("evo/node.rs"), "names V's site: {e}");
        let ambiguous = Some(Binding::Ambiguous(vec!["x".into()]));
        assert!(mnhf_gate(&inv, &Ok(1), false, &ambiguous).is_err(), "not unique");
    }

    /// OD-L3-16's leg witness: a leg whose V load is refused stops before its first op with
    /// the load's status, and has no trial results.
    #[test]
    fn a_refused_v_load_stops_the_leg_before_op_0() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let inventory = Inventory::load().expect("the fixture");
        let dashd = Dashd {
            version: "24.0.0-rc.5".into(),
            binary: dir.path().join("never-started"),
            line: String::new(),
            sha256: String::new(),
            tag: inventory.provenance.tag.clone(),
            commit: inventory.provenance.commit.clone(),
        };
        let config = LegConfig {
            dashd: &dashd,
            set: Set::Tier1,
            par: Par::One,
            inventory: &inventory,
            only: None,
            record: false,
            fixture: None,
        };
        let mut args = chain_args(Par::One);
        args.push("-sporkkey=nonsense".into());
        let log = run_on(&config, dir.path(), args);
        assert_eq!(log.load_refused.as_deref(), Some("Unable to sign spork message, wrong key?"));
        assert_eq!(
            log.error.as_deref(),
            Some("V's load was refused: Unable to sign spork message, wrong key?")
        );
        assert!(log.ops.is_empty() && log.trials.is_empty() && log.dashd_argv.is_empty());
        // Its baseline: the same leg's V on the leg's own arguments serves.
        let v = NodeSide::with_argv(
            &chain_args(Par::One),
            T0,
            &private_dir(dir.path(), "ok").expect("a dir"),
            None,
        )
        .expect("a load");
        assert_eq!(v.down(), None);
    }

    /// r2-m1: L3's own operators, and only they, get standard funding.
    #[test]
    fn only_l3_operators_get_standard_funding() {
        for par in [Par::One, Par::Four] {
            let standard: Vec<&str> = cases(Set::Full, par)
                .expect("the full set's cases")
                .iter()
                .filter(|c| c.standard_funding())
                .map(|c| c.id.op)
                .collect();
            let mut ops = standard.clone();
            ops.dedup();
            assert_eq!(ops, L3_OPERATORS.iter().map(|o| o.name).collect::<Vec<_>>());
        }
    }

    /// The full set puts each case on its site's heights: `Reg` from [`CASE_BASE`] below the
    /// budget start, `BO` from [`BO_BASE`], a superblock site's cases at its height; the split
    /// comes before every case, and the mints fit the budget window, each mature before its case.
    #[test]
    fn every_case_is_on_its_site_and_funded_before_it() {
        for set in [Set::Tier1, Set::Full] {
            let all = cases(set, Par::One).expect("the set's cases");
            for c in &all {
                assert!(c.id.site.holds(c.height), "{} at {}", c.id, c.height);
                assert!(set == Set::Full || c.id.site.tier1(), "{}", c.id);
                assert!(SPLIT_HEIGHT + 2 < c.height, "{}", c.id);
            }
            let (of, heights) = mints(&all).expect("the mints fit");
            for (i, m) in &of {
                for k in m {
                    assert!(heights[*k] + build::MATURITY <= all[*i].height, "{}", all[*i].id);
                }
            }
            assert_eq!(of.is_empty(), set == Set::Tier1);
        }
        let full = cases(Set::Full, Par::One).expect("the full set's cases");
        let at = |id: &str| full.iter().find(|c| c.id.to_string() == id).map(|c| c.height);
        assert_eq!(at("cb_value.bound+1@BW/par1"), Some(1_002));
        assert_eq!(at("cb_value.bound+1@BO/par1"), Some(BO_BASE + 2));
        assert_eq!(at("cb_value.max+1@SB1520/par1"), Some(1_520));
        assert_eq!(at("cb_value.bound@SO/par1"), Some(1_502));
        assert_eq!(Set::parse("full"), Ok(Set::Full));
        assert!(Set::parse("tier2").is_err());
    }

    /// rev5 §9: a funding read through the mempool overlay is refused.
    #[test]
    fn funding_reads_are_chain_only() {
        assert_eq!(planes::chain_only("gettxout", &json!(["00", 0, false])), Ok(()));
        assert!(planes::chain_only("gettxout", &json!(["00", 0, true])).is_err());
        assert!(
            planes::chain_only("gettxout", &json!(["00", 0])).is_err(),
            "Core's default is true"
        );
        assert_eq!(planes::chain_only("getblock", &json!(["00", 0])), Ok(()));
    }
}
