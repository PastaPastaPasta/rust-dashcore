//! O10-2's pairs (O10 plan §4.4, §4.5): two defects in one block, the model's prediction of
//! which Core reports, and each defect's solo control.
//!
//! A pair is built from parts on one fresh template:
//! - two catalogue variants ([`Build::Compose`]), each applied to the template with its own
//!   coins, then merged: the header fields and coinbase each sets, and their transactions in the
//!   pair's order, optionally with clean filler transactions between them;
//! - one transaction with two defects ([`Build::SameTx`]: classes 1, 3 and 4, W3, W3b);
//! - an a10 scenario whose last block also carries a header defect ([`Build::Scenario`],
//!   class 7), run by the leg's scenario runner.
//!
//! A defect's solo control is the same block with the other defect removed: its header field
//! and coinbase back to the template's, and each of its transactions replaced by a clean twin
//! with as many inputs and outputs (or, in one transaction, the same inputs and outputs with
//! that defect undone). The solo keeps the pair's domains in its bytes, checked on the
//! measurement, and is predicted under the pair's measured setting (DEC-998 M-1).
//!
//! The generator ([`generated`]) dry-runs every candidate on a synthetic chain before any leg
//! starts. It measures the pair and both solos from their bytes ([`super::measure`]), requires
//! the pair's instances to be exactly the solos' together (a re-measured precondition: a
//! composition that breaks one, or adds a defect, is refused), places them on the model, and
//! counts every refusal by its typed reason: infeasible facts, unreachable after a return, an
//! iteration-guard conflict (item 8b), a footprint overlap, an ambiguous binding. Nothing it
//! refuses becomes a firing, a `None` or a failure.
//!
//! [`generated`]: super::pairset::generated

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Mutex, OnceLock};

use super::footprint::{self, BLOCK_TXS, CHAIN, COIN_REUSE};
use super::inventory::{Binding, Facts as BindFacts, Inventory, ParentState};
use super::ledger::{Ledger, is_coinbase};
use super::measure::{self, Measured, Place};
use super::precedence::{self, At, Facts, Instance, Model, Unplaced};
use crate::chain::build::{self, BlockPlan, Input, Lock, OP_RETURN, SIGOPS_MAX, TxPlan, push};
use crate::chain::mutate::{
    Carrier, Cx, EXACT_COINBASE, Expect, L3Cx, MAX_MONEY, Mode, Operator, Par, REUSE_SETUP, Site,
    Stage, Variant, catalogue, max_money_coins, scenario_steps,
};
use crate::chain::world::Coin;

/// The order of a composed pair's transactions: `a`'s first, or `b`'s.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Order {
    Ab,
    Ba,
}

/// A catalogue variant as a pair's part.
pub type Part = (&'static Operator, &'static Variant);

/// One transaction with two defects (classes 1, 3, 4, 10).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SameTx {
    /// W3: inputs [MAX, MAX, immature]: the range at input 1 before the immature at 2.
    W3,
    /// W3 reversed: [immature, MAX, MAX]: the immature at input 0 first.
    W3Reversed,
    /// W3b: [MAX (mature), immature]: at input 1 the maturity check before the value's range.
    W3b,
    /// Class 1: value out over value in, and an unmet relative lock (CheckTxInputs before
    /// SequenceLocks, v:2709 before v:2742). M4's.
    BelowOutBip68,
    /// Class 1: an immature input and an unmet relative lock.
    ImmatureBip68,
    /// Class 3: a non-final lock time and value out over value in (ContextualCheckBlock's loop
    /// before ConnectBlock's).
    NonfinalBelowOut,
    /// Class 3: a negative output and a non-final lock time (CheckBlock's loop first).
    NegativeNonfinal,
    /// Class 3: a negative output and value out over value in.
    NegativeBelowOut,
    /// Class 10 at P1 (OD-L3-31): a non-final lock time and 20,001 legacy sigops, the block's
    /// one transaction: ContextualCheckBlock's loop (v:4569-4582) before its sigops (v:4585),
    /// the loop-body defect at index 1, the last.
    NonfinalSigops,
}

impl SameTx {
    pub const ALL: [SameTx; 9] = [
        SameTx::W3,
        SameTx::W3Reversed,
        SameTx::W3b,
        SameTx::BelowOutBip68,
        SameTx::ImmatureBip68,
        SameTx::NonfinalBelowOut,
        SameTx::NegativeNonfinal,
        SameTx::NegativeBelowOut,
        SameTx::NonfinalSigops,
    ];

    pub fn name(self) -> &'static str {
        match self {
            SameTx::W3 => "w3",
            SameTx::W3Reversed => "w3_reversed",
            SameTx::W3b => "w3b",
            SameTx::BelowOutBip68 => "below_out~bip68",
            SameTx::ImmatureBip68 => "immature~bip68",
            SameTx::NonfinalBelowOut => "nonfinal~below_out",
            SameTx::NegativeNonfinal => "vout_negative~nonfinal",
            SameTx::NegativeBelowOut => "vout_negative~below_out",
            SameTx::NonfinalSigops => "nonfinal~sigops_ctx",
        }
    }

    pub fn class(self) -> u8 {
        match self {
            SameTx::W3 | SameTx::W3Reversed | SameTx::W3b => 4,
            SameTx::BelowOutBip68 | SameTx::ImmatureBip68 => 1,
            SameTx::NonfinalSigops => 10,
            _ => 3,
        }
    }

    pub fn site(self) -> Site {
        match self {
            SameTx::W3 | SameTx::W3Reversed | SameTx::W3b => Site::BO,
            SameTx::NonfinalSigops => Site::P1,
            _ => Site::Reg,
        }
    }

    /// The leg's `MAX_MONEY` mints it spends.
    fn mints(self) -> usize {
        match self {
            SameTx::W3 | SameTx::W3Reversed => 2,
            SameTx::W3b => 1,
            _ => 0,
        }
    }

    /// The transaction with each defect present where `keep` says so.
    fn build(self, w: &World<'_>, keep: [bool; 2]) -> TxPlan {
        use Src::{Case, Immature, Mature, Max};
        let pick = |on: bool, yes: Src, no: Src| {
            if on {
                yes
            } else {
                no
            }
        };
        // Each defect: the inputs it needs, then the output and field changes.
        let (inputs, below, bip68, nonfinal, negative) = match self {
            SameTx::W3 => (
                vec![
                    pick(keep[0], Max(0), Case(0)),
                    pick(keep[0], Max(1), Case(1)),
                    pick(keep[1], Immature, Mature),
                ],
                false,
                false,
                false,
                false,
            ),
            SameTx::W3Reversed => (
                vec![
                    pick(keep[0], Immature, Mature),
                    pick(keep[1], Max(0), Case(0)),
                    pick(keep[1], Max(1), Case(1)),
                ],
                false,
                false,
                false,
                false,
            ),
            SameTx::W3b => (
                vec![pick(keep[1], Max(0), Case(0)), pick(keep[0], Immature, Mature)],
                false,
                false,
                false,
                false,
            ),
            SameTx::BelowOutBip68 => (vec![Case(0)], keep[0], keep[1], false, false),
            SameTx::ImmatureBip68 => {
                (vec![pick(keep[0], Immature, Mature)], false, keep[1], false, false)
            }
            SameTx::NonfinalBelowOut => (vec![Case(0)], keep[1], false, keep[0], false),
            SameTx::NegativeNonfinal => (vec![Case(0)], false, false, keep[1], keep[0]),
            SameTx::NegativeBelowOut => (vec![Case(0)], keep[1], false, false, keep[0]),
            SameTx::NonfinalSigops => (vec![Case(0)], false, false, keep[0], false),
        };
        let coins: Vec<Coin> = inputs.iter().map(|s| w.source(*s)).collect();
        let value_in = coins.iter().fold(0i64, |s, c| s.saturating_add(c.value)).min(MAX_MONEY);
        let mut tx = TxPlan::simple(
            coins.iter().map(|c| Input::new(c.outpoint, c.lock.script_sig())).collect(),
            Vec::new(),
        );
        // Value out one over value in (with a negative output's -1 offset), or a 1,000 fee.
        let main = match (below, negative) {
            (true, true) => value_in + 2,
            (true, false) => value_in + 1,
            (false, _) => value_in - 1_000,
        };
        // A shape with a negative output keeps that output in every block (S-F1: the solos keep
        // the pair's outputs): -1 with the defect, 0 without.
        if matches!(self, SameTx::NegativeNonfinal | SameTx::NegativeBelowOut) {
            tx.outputs.push((
                if negative {
                    -1
                } else {
                    0
                },
                Lock::True.script_pubkey(),
            ));
        }
        tx.outputs.push((main, Lock::True.script_pubkey()));
        if self == SameTx::NonfinalSigops {
            // mutate.rs's sigops_ctx: 20,001 legacy sigops, over the pre-DIP0001 20,000
            // (v:4585); without the defect 20,000 in as many outputs (S-F1: the same shape).
            for k in [10_000, 10_000, usize::from(keep[1])] {
                tx.outputs.push((0, build::legacy_sigops(k)));
            }
        }
        if bip68 {
            // tx_verify.cpp:84-97: a 65,535-block relative lock on input 0 never holds here.
            tx.version = 2;
            tx.inputs[0].sequence = 0x0000_ffff;
        }
        if nonfinal {
            // tx_verify.cpp:17-35: a height lock at the block's height, a non-final sequence.
            tx.lock_time = w.height;
            for i in &mut tx.inputs {
                if i.sequence == u32::MAX {
                    i.sequence = u32::MAX - 1;
                }
            }
        }
        tx.outputs.push((0, [vec![OP_RETURN], push(&w.trial)].concat()));
        tx
    }
}

/// A same-transaction input's coin.
#[derive(Clone, Copy, Debug)]
enum Src {
    Case(usize),
    Max(usize),
    Immature,
    Mature,
}

/// How a pair's block is built.
#[derive(Clone, Copy)]
pub enum Build {
    Compose {
        a: Part,
        b: Part,
        order: Order,
        /// Clean filler transactions between the two parts' transactions.
        fill: usize,
    },
    SameTx(SameTx),
    /// A scenario (`a`) whose last step's block also takes `b`'s header field.
    Scenario {
        a: Part,
        b: Part,
    },
}

/// A pair case's plan, its class and its solos.
pub struct Pair {
    /// `<a>~<b>.<placement>`, or `same.<shape>`.
    pub id: &'static str,
    pub build: Build,
    pub site: Site,
    /// §4.5's class (1-10).
    pub class: u8,
    /// The §4.4 witness it is (W1-W6, W3b, `.reversed`), if any.
    pub witness: Option<&'static str>,
    /// The V mutants (M1-M10) it is built to kill.
    pub mutants: &'static [&'static str],
    /// Each solo's case id ([`Role::Solo`]): equal ids are one case (deduplicated by variant,
    /// placement and the pair's setting).
    pub solos: [&'static str; 2],
}

/// Which block of a pair a case submits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Pair,
    /// The pair with part `k`'s defect alone.
    Solo(usize),
}

/// An O10 case's plan: its pair and role.
#[derive(Clone, Copy)]
pub struct O10 {
    pub pair: &'static Pair,
    pub role: Role,
}

impl O10 {
    /// The case id's operator part.
    pub fn op(&self) -> &'static str {
        match (self.role, self.pair.witness) {
            (Role::Pair, Some(_)) => "o10.w",
            (Role::Pair, None) => "o10.pair",
            (Role::Solo(_), _) => "o10.solo",
        }
    }

    /// The §4.4 witness the case is: the pair's, or W4 for W4.reversed's script solo (tx1's
    /// script fault, tx2 clean).
    pub fn witness(&self) -> Option<&'static str> {
        match (self.role, self.pair.witness) {
            (Role::Pair, w) => w,
            (Role::Solo(0), Some("W4.reversed")) => Some("W4"),
            _ => None,
        }
    }

    /// The case id's variant part.
    pub fn variant(&self) -> &'static str {
        match self.role {
            Role::Pair => self.pair.witness.unwrap_or(self.pair.id),
            Role::Solo(k) => self.pair.solos[k],
        }
    }

    pub fn locks(&self) -> Vec<Lock> {
        self.pair.locks()
    }

    pub fn mints(&self) -> usize {
        self.pair.mints()
    }
}

impl Pair {
    /// The split coins' locks: three per part, three free (fillers, clean twins). A composed
    /// part that funds its own spend has that coin under the fund's lock ([`prefund`]).
    pub fn locks(&self) -> Vec<Lock> {
        match self.build {
            Build::Compose {
                a,
                b,
                ..
            } => {
                let mut locks: Vec<Lock> = [split_lock(a.0), split_lock(b.0), Lock::P2shTrue]
                    .into_iter()
                    .flat_map(|l| std::iter::repeat_n(l, 3))
                    .collect();
                for (k, part) in [a, b].into_iter().enumerate() {
                    if let Some((c, lock)) = prefund(part, self.site) {
                        locks[3 * k + c] = lock;
                    }
                }
                locks
            }
            Build::Scenario {
                a,
                b,
            } => [split_lock(a.0), split_lock(b.0), Lock::P2shTrue]
                .into_iter()
                .flat_map(|l| std::iter::repeat_n(l, 3))
                .collect(),
            Build::SameTx(_) => vec![Lock::P2shTrue; 3],
        }
    }

    /// The leg's `MAX_MONEY` mints it spends: each part's, in part order.
    pub fn mints(&self) -> usize {
        match self.build {
            Build::Compose {
                a,
                b,
                ..
            }
            | Build::Scenario {
                a,
                b,
            } => part_mints(a, self.site) + part_mints(b, self.site),
            Build::SameTx(s) => s.mints(),
        }
    }
}

/// A part's split coins' lock: as a10 funds its operator.
fn split_lock(op: &Operator) -> Lock {
    if crate::chain::mutate::L3_OPERATORS.iter().any(|o| o.name == op.name) {
        Lock::P2shTrue
    } else {
        Lock::True
    }
}

/// In a part's transactions, one that funds a later one alone (`locked_spend`'s and the sigop
/// filler's fund and spend): `(coin, fund, spend)`, `fund` spending the part's coin `coin`
/// alone into one output (and the carrier) that `spend`, alone of `txs`, spends alone.
fn funding(txs: &[TxPlan], coins: &[Coin]) -> Option<(usize, usize, usize)> {
    txs.iter().enumerate().find_map(|(f, fund)| {
        let [input] = &fund.inputs[..] else {
            return None;
        };
        let c = coins.iter().position(|c| c.outpoint == input.prevout)?;
        let carrier = |o: &(i64, Vec<u8>)| o.1.first() == Some(&OP_RETURN);
        if carrier(fund.outputs.first()?) || fund.outputs.iter().skip(1).any(|o| !carrier(o)) {
            return None;
        }
        let txid = fund.txid();
        let spenders: Vec<usize> =
            (0..txs.len()).filter(|s| txs[*s].inputs.iter().any(|i| i.prevout.0 == txid)).collect();
        match spenders[..] {
            [s] if txs[s].inputs.len() == 1 && txs[s].inputs[0].prevout == (txid, 0) => {
                Some((c, f, s))
            }
            _ => None,
        }
    })
}

/// The height-free lock whose scriptPubKey is `script` (`Cltv` reads the case's height, so it
/// is never one).
fn lock_of(script: &[u8]) -> Option<Lock> {
    use Lock::*;
    [False, NonDer, BadSig, NullDummy, P2shFalse, P2pkhBadSig, P2shBadSig, NonDerNot, Csv(10)]
        .into_iter()
        .chain((0..=SIGOPS_MAX).map(Sigops))
        .find(|l| l.script_pubkey() == script)
}

/// Whether `part` funds its own spend in its block, and if so which of its split coins the fund
/// spends and the lock the fund's output carries (Sol r1 S-F2). O10 locks that coin so at the
/// split and its block spends it directly (`collapse`): the spend's defect is then at the
/// part's first transaction, where §4.4 and §4.5 place it. `None` for a part on mints, or one
/// whose fund's lock reads the height.
pub fn prefund(part: Part, site: Site) -> Option<(usize, Lock)> {
    type Found = Option<(usize, Lock)>;
    static CACHE: OnceLock<Mutex<BTreeMap<(String, Site), Found>>> = OnceLock::new();
    let key = (part_id(part), site);
    if let Some(found) = CACHE.get_or_init(Default::default).lock().expect("the cache").get(&key) {
        return *found;
    }
    let found = (part_mints(part, site) == 0)
        .then(|| {
            let dry = Dry::new(site, &[split_lock(part.0); 9], 0);
            let world = dry.world(*b"o10-fund");
            let coins = world.part_coins(0, [part, part], site);
            let d = delta(world.template, &world.applied(part, &coins, site)).ok()?;
            let (c, f, _) = funding(&d.txs, &coins)?;
            Some((c, lock_of(&d.txs[f].outputs[0].1)?))
        })
        .flatten();
    CACHE.get_or_init(Default::default).lock().expect("the cache").insert(key, found);
    found
}

/// A [`prefund`]ed part's transactions: its fund dropped, its spend spending the coin the split
/// locked as the fund's output was.
fn collapse(txs: &mut Vec<TxPlan>, coins: &[Coin]) -> Result<(), String> {
    let (c, f, s) = funding(txs, coins).ok_or("a prefunded part with no fund and spend")?;
    if coins[c].lock.script_pubkey() != txs[f].outputs[0].1 {
        return Err("a prefunded coin not under its fund's lock".into());
    }
    txs[s].inputs[0].prevout = coins[c].outpoint;
    txs.remove(f);
    Ok(())
}

pub fn part_mints(p: Part, site: Site) -> usize {
    max_money_coins(p.0.name, p.1.name, site)
}

pub fn part_id(p: Part) -> String {
    footprint::variant_id(p.0, p.1)
}

/// What building a pair's blocks reads: the template and the chain at its height, and the
/// case's coins.
pub struct World<'a> {
    pub template: &'a BlockPlan,
    pub height: u32,
    pub mtp: u32,
    pub mocktime: i64,
    pub reward: i64,
    pub sb_limit: i64,
    /// The case's split coins ([`O10::locks`]).
    pub coins: &'a [Coin],
    /// Its mints, each part's in part order.
    pub mints: &'a [Coin],
    pub immature: Coin,
    pub mature: Coin,
    pub spent: ([u8; 32], u32),
    pub reuse: &'a [TxPlan],
    pub coin_mtp: u32,
    pub trial: [u8; 8],
}

impl World<'_> {
    fn source(&self, s: Src) -> Coin {
        match s {
            Src::Case(k) => self.coins[k],
            Src::Max(k) => self.mints[k],
            Src::Immature => self.immature,
            Src::Mature => self.mature,
        }
    }

    /// Part `k`'s coins: its mints if it has any, else its three split coins.
    fn part_coins(&self, k: usize, parts: [Part; 2], site: Site) -> Vec<Coin> {
        let before: usize = parts[..k].iter().map(|p| part_mints(*p, site)).sum();
        match part_mints(parts[k], site) {
            0 => self.coins[3 * k..3 * k + 3].to_vec(),
            n => self.mints[before..before + n].to_vec(),
        }
    }

    /// `part` applied alone to the template with its coins.
    fn applied(&self, part: Part, coins: &[Coin], site: Site) -> BlockPlan {
        let l3 = L3Cx {
            trial: self.trial,
            carrier: part.1.carrier,
            variant: part.1.name,
            site,
            reward: self.reward,
            sb_limit: self.sb_limit,
            spent: self.spent,
            mature: self.mature,
            coin_mtp: self.coin_mtp,
            reuse: self.reuse,
        };
        let cx = Cx {
            height: self.height,
            mtp: self.mtp,
            mocktime: self.mocktime,
            coins,
            immature: Some(self.immature),
            l3: Some(&l3),
        };
        let mut plan = self.template.clone();
        (part.0.apply)(&mut plan, &cx);
        plan
    }

    /// `part` alone on the template, on the coins it would have as a pair's first part, as a10
    /// builds it (before the leg's coinbase tag).
    pub fn part(&self, part: Part, site: Site) -> BlockPlan {
        self.applied(part, &self.part_coins(0, [part, part], site), site)
    }

    /// Clean twins of a removed part's transactions `of` (S-F1): one each, with as many inputs and
    /// outputs as its original (one where the original's is empty: that emptiness is the
    /// defect), spending free coin `k` and the part's own split coins `own` (unspent in its
    /// solo; a prefunded coin's lock excepted), then the twins' outputs, oldest first. Each pays a
    /// 1,000-satoshi fee; with two or more outputs its last is the trial's carrier.
    fn twins(&self, k: usize, own: &[Coin], of: &[TxPlan]) -> Result<Vec<TxPlan>, String> {
        let spendable = |c: &&Coin| matches!(c.lock, Lock::True | Lock::P2shTrue);
        let mut pool: VecDeque<Coin> = std::iter::once(&self.coins[6 + k])
            .chain(own.iter().filter(spendable))
            .copied()
            .collect();
        let mut out = Vec::new();
        for original in of {
            let (ins, outs) = (original.inputs.len().max(1), original.outputs.len().max(1));
            if pool.len() < ins {
                return Err(format!("a clean twin needs {ins} coins, {} left", pool.len()));
            }
            let spent: Vec<_> = pool.drain(..ins).collect();
            let value_in = spent.iter().map(|c| c.value).sum::<i64>();
            let values = if outs == 1 {
                1
            } else {
                outs - 1
            };
            let each = (value_in - 1_000) / values as i64;
            if each <= 0 {
                return Err(format!("a clean twin's {values} outputs of {value_in}"));
            }
            let mut tx = TxPlan::simple(
                spent.iter().map(|c| Input::new(c.outpoint, c.lock.script_sig())).collect(),
                vec![(each, Lock::True.script_pubkey()); values],
            );
            if outs > 1 {
                tx.outputs.push((0, [vec![OP_RETURN], push(&self.trial)].concat()));
            }
            let txid = tx.txid();
            pool.extend((0..values).map(|n| Coin {
                outpoint: (txid, n as u32),
                value: each,
                lock: Lock::True,
                height: self.height,
                coinbase: false,
            }));
            out.push(tx);
        }
        Ok(out)
    }

    /// `n` clean transactions: a chain from free coin `k` (each spends the one before, one
    /// `OP_TRUE` output, a 1,000-satoshi fee, the trial's carrier).
    fn clean(&self, k: usize, n: usize) -> Vec<TxPlan> {
        let free = self.coins[6 + k];
        let mut out: Vec<TxPlan> = Vec::new();
        let (mut prevout, mut value, mut script_sig) =
            (free.outpoint, free.value, free.lock.script_sig());
        for _ in 0..n {
            let mut tx = TxPlan::simple(
                vec![Input::new(prevout, script_sig)],
                vec![(value - 1_000, Lock::True.script_pubkey())],
            );
            tx.outputs.push((0, [vec![OP_RETURN], push(&self.trial)].concat()));
            prevout = (tx.txid(), 0);
            value -= 1_000;
            script_sig = Vec::new();
            out.push(tx);
        }
        out
    }
}

/// A pair's three blocks on one world.
#[derive(Clone, Debug)]
pub struct Blocks {
    pub pair: BlockPlan,
    pub solos: [BlockPlan; 2],
}

impl Blocks {
    pub fn of(&self, role: Role) -> &BlockPlan {
        match role {
            Role::Pair => &self.pair,
            Role::Solo(k) => &self.solos[k],
        }
    }
}

/// A part's change to the template: the header fields and coinbase it sets, its transactions.
struct Delta {
    version: Option<i32>,
    time: Option<u32>,
    bits: Option<u32>,
    merkle: Option<[u8; 32]>,
    coinbase: Option<TxPlan>,
    txs: Vec<TxPlan>,
}

fn delta(template: &BlockPlan, applied: &BlockPlan) -> Result<Delta, String> {
    if applied.prev != template.prev || !applied.txs.first().is_some_and(is_coinbase) {
        return Err("a part changes the parent or drops the coinbase".into());
    }
    Ok(Delta {
        version: (applied.version != template.version).then_some(applied.version),
        time: (applied.time != template.time).then_some(applied.time),
        bits: (applied.bits != template.bits).then_some(applied.bits),
        merkle: applied.merkle_override,
        coinbase: (applied.txs[0] != template.txs[0]).then(|| applied.txs[0].clone()),
        txs: applied.txs[1..].to_vec(),
    })
}

/// Two deltas on the template in `order`, `fill` between them.
fn merge(
    template: &BlockPlan,
    a: &Delta,
    b: &Delta,
    order: Order,
    fill: &[TxPlan],
) -> Result<BlockPlan, String> {
    fn one<T: Copy>(field: &str, a: Option<T>, b: Option<T>, base: T) -> Result<T, String> {
        match (a, b) {
            (Some(_), Some(_)) => Err(format!("both parts set the {field}")),
            (x, y) => Ok(x.or(y).unwrap_or(base)),
        }
    }
    let coinbase = match (&a.coinbase, &b.coinbase) {
        (Some(_), Some(_)) => return Err("both parts set the coinbase".into()),
        (x, y) => x.as_ref().or(y.as_ref()).unwrap_or(&template.txs[0]).clone(),
    };
    let (first, second) = match order {
        Order::Ab => (&a.txs, &b.txs),
        Order::Ba => (&b.txs, &a.txs),
    };
    let mut txs = vec![coinbase];
    txs.extend(first.iter().cloned());
    txs.extend(fill.iter().cloned());
    txs.extend(second.iter().cloned());
    Ok(BlockPlan {
        prev: template.prev,
        version: one("version", a.version, b.version, template.version)?,
        time: one("time", a.time, b.time, template.time)?,
        bits: one("bits", a.bits, b.bits, template.bits)?,
        merkle_override: match (a.merkle, b.merkle) {
            (Some(_), Some(_)) => return Err("both parts set the merkle root".into()),
            (x, y) => x.or(y),
        },
        txs,
    })
}

/// A part's clean twin: no header field, no coinbase, `clean` for its transactions.
fn cleaned(clean: Vec<TxPlan>) -> Delta {
    Delta {
        version: None,
        time: None,
        bits: None,
        merkle: None,
        coinbase: None,
        txs: clean,
    }
}

/// Add the trial's coinbase tag, as the leg does after an operator's `apply` (a10's
/// `prepare`), unless a part's coinbase is exact.
pub fn tag_coinbase(plan: &mut BlockPlan, trial: &[u8; 8], exact: bool) {
    if !exact && plan.txs.first().is_some_and(is_coinbase) {
        plan.txs[0].inputs[0].script_sig.extend(push(trial));
    }
}

fn exact(p: Part) -> bool {
    EXACT_COINBASE.contains(&p.0.name)
}

/// The pair's blocks on `w` (not [`Build::Scenario`]'s: the leg's runner builds those).
pub fn build(pair: &Pair, w: &World<'_>) -> Result<Blocks, String> {
    let (mut blocks, exact) = match pair.build {
        Build::Compose {
            a,
            b,
            order,
            fill,
        } => {
            let parts = [a, b];
            let part = |k: usize| -> Result<Delta, String> {
                let coins = w.part_coins(k, parts, pair.site);
                let mut d = delta(w.template, &w.applied(parts[k], &coins, pair.site))?;
                if prefund(parts[k], pair.site).is_some() {
                    collapse(&mut d.txs, &coins)?;
                }
                Ok(d)
            };
            let (da, db) = (part(0)?, part(1)?);
            let fill = w.clean(1, fill);
            // The clean twins: free coin 0 and a's own coins for part a's, 2 and b's for b's; free
            // coin 1's chain for the filler.
            let own = |k: usize| &w.coins[3 * k..3 * k + 3];
            let ca = cleaned(w.twins(0, own(0), &da.txs)?);
            let cb = cleaned(w.twins(2, own(1), &db.txs)?);
            (
                Blocks {
                    pair: merge(w.template, &da, &db, order, &fill)?,
                    solos: [
                        merge(w.template, &da, &cb, order, &fill)?,
                        merge(w.template, &ca, &db, order, &fill)?,
                    ],
                },
                exact(a) || exact(b),
            )
        }
        Build::SameTx(s) => {
            let block = |keep| BlockPlan {
                txs: vec![w.template.txs[0].clone(), s.build(w, keep)],
                ..w.template.clone()
            };
            (
                Blocks {
                    pair: block([true, true]),
                    solos: [block([true, false]), block([false, true])],
                },
                false,
            )
        }
        Build::Scenario {
            ..
        } => return Err(format!("{}: a scenario pair is the leg runner's", pair.id)),
    };
    for plan in std::iter::once(&mut blocks.pair).chain(blocks.solos.iter_mut()) {
        tag_coinbase(plan, &w.trial, exact);
    }
    Ok(blocks)
}

/// The measured instances of a pair's three blocks, and the check that the pair's are exactly
/// its solos' together, each solo with a defect (§4.5: a re-measured precondition).
pub struct Measures {
    pub pair: Measured,
    pub solos: [Measured; 2],
}

/// A block's measured defects and each transaction's (inputs, outputs): a control's domains.
pub type Domains<'a> = (&'a [(String, At)], &'a [(usize, usize)]);

/// Whether `solo`, a control of `pair`, keeps the pair's transactions and each one's input and
/// output counts (S-F1); a domain the solo's undone defect empties takes one. A control
/// credited to a pair it was not built from is checked the same way (R2-F1).
pub fn keeps_domains(pair: Domains<'_>, solo: Domains<'_>) -> Result<(), String> {
    if solo.1.len() != pair.1.len() {
        return Err("changes the transaction count".into());
    }
    for (i, (got, want)) in solo.1.iter().zip(pair.1).enumerate() {
        let undone = pair.0.iter().any(|d| d.1.vtx == Some(i) && !solo.0.contains(d));
        let fits = |g: usize, w: usize| g == w || (undone && w == 0 && g == 1);
        if !fits(got.0, want.0) || !fits(got.1, want.1) {
            return Err(format!("changes tx {i}'s inputs and outputs from {want:?} to {got:?}"));
        }
    }
    Ok(())
}

/// Why [`measures`] refuses a pair's blocks (Opus r1 m1): an unsound composition is a typed
/// refusal; a block measure cannot read (an oracle failure, an undecidable fact) is a harness
/// error, never an absent defect, and fails whoever asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MeasureError {
    Composition(String),
    Measurement(String),
}

impl std::fmt::Display for MeasureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MeasureError::Composition(e) => write!(f, "broken composition: {e}"),
            MeasureError::Measurement(e) => write!(f, "measurement: {e}"),
        }
    }
}

pub fn measures(blocks: &Blocks, place: &Place<'_>) -> Result<Measures, MeasureError> {
    let m =
        |p: &BlockPlan| measure::measure(p, &p.build(), place).map_err(MeasureError::Measurement);
    let broken = MeasureError::Composition;
    let out = Measures {
        pair: m(&blocks.pair)?,
        solos: [m(&blocks.solos[0])?, m(&blocks.solos[1])?],
    };
    let set = |m: &Measured| m.defects.iter().cloned().collect::<BTreeSet<_>>();
    let union: BTreeSet<_> = set(&out.solos[0]).union(&set(&out.solos[1])).cloned().collect();
    if set(&out.pair) != union {
        return Err(broken(format!(
            "the pair measures {:?}, its solos {:?} and {:?}",
            out.pair.defects, out.solos[0].defects, out.solos[1].defects
        )));
    }
    let vtx = |m: &Measured| m.facts.lengths.get("block.vtx").copied();
    for (k, s) in out.solos.iter().enumerate() {
        if vtx(s) != vtx(&out.pair) {
            return Err(broken(format!("solo {k} changes the transaction count")));
        }
        if s.defects.is_empty() {
            return Err(broken(format!("solo {k} measures no defect")));
        }
        // S-F1: each transaction keeps its input and output counts, so the solo's loops run
        // over the pair's domains.
        keeps_domains((&out.pair.defects, &out.pair.shapes), (&s.defects, &s.shapes))
            .map_err(|e| broken(format!("solo {k} {e}")))?;
        if set(s).is_subset(&set(&out.solos[1 - k])) {
            return Err(broken(format!("solo {k}'s defects are the other's")));
        }
    }
    Ok(out)
}

pub fn interned(s: &str) -> &'static str {
    static POOL: OnceLock<Mutex<BTreeSet<&'static str>>> = OnceLock::new();
    let mut pool = POOL.get_or_init(Default::default).lock().expect("the pool");
    if let Some(found) = pool.get(s) {
        return found;
    }
    let leaked: &'static str = Box::leak(s.to_owned().into_boxed_str());
    pool.insert(leaked);
    leaked
}

/// A row key's reject reason prefix: its template up to its first format directive.
pub fn reason(row: &str) -> &str {
    let template = row.rsplit_once(':').map_or(row, |(_, t)| t);
    template.split('%').next().unwrap_or(template)
}

/// The walker's answer as an expectation (a fired row is its reject; none fired, accepted).
pub fn expectation(fired: &Option<(String, bool)>) -> Expect {
    match fired {
        None => Expect::Accept,
        Some((row, _)) => Expect::Reject {
            reason: interned(reason(row)),
            occurrence: interned(row),
        },
    }
}

/// The instances a measured block holds on the model, each with its part's footprint.
pub fn setting_of(
    model: &Model,
    mode: Mode,
    par: Par,
    m: &Measured,
    under: &Facts,
) -> Result<(Vec<Instance>, Facts), Unplaced> {
    let defects: Vec<(&str, At)> = m.defects.iter().map(|(r, a)| (r.as_str(), *a)).collect();
    let extra = |f: &mut Facts| {
        for (k, v) in &under.iter {
            f.iter.insert(k.clone(), v.clone());
        }
        for (k, v) in &under.facts {
            f.facts.insert(k.clone(), *v);
        }
    };
    // The walk's domain lengths: the instances' shape, at least the block's own (`setting`
    // derives the shape after `extra`).
    let (instances, mut facts) =
        precedence::setting(model, mode, par, &defects, Facts::default(), &extra)?;
    for (k, v) in &under.lengths {
        let n = facts.lengths.entry(k.clone()).or_default();
        *n = (*n).max(*v);
    }
    Ok((instances, facts))
}

/// The model's answer for `role`'s block: the pair's instances, or a solo's under the pair's
/// measured setting (M-1).
pub fn predict(
    model: &Model,
    mode: Mode,
    par: Par,
    m: &Measures,
    role: Role,
) -> precedence::Answer {
    let block = match role {
        Role::Pair => &m.pair,
        Role::Solo(k) => &m.solos[k],
    };
    let (instances, facts) = setting_of(model, mode, par, block, &m.pair.facts)?;
    Ok(precedence::walked(model, mode, par, &instances, &facts))
}

/// The operator an O10 case's [`super::leg::Case`] names: no catalogue row (its blocks are
/// [`build()`]'s, its expectations the walker's, [`O10`]).
pub static O10_OPERATOR: Operator = Operator {
    name: "o10",
    rejects: &[],
    stage: Stage::Header,
    par: false,
    apply: |_, _| {},
    variants: &[Variant {
        name: "o10",
        sites: &[Site::Reg, Site::BO, Site::P1],
        expect: |_, _, _| Expect::Accept,
        carrier: Carrier::OpReturn,
        full_only: false,
    }],
};

/// A class 7 pair's last step (§4.5): the scenario's block or header there, also off `b`'s
/// header field.
pub fn scenario_last(b: Part, plan: &mut BlockPlan, height: u32, mtp: u32, mocktime: i64) {
    let cx = Cx {
        height,
        mtp,
        mocktime,
        coins: &[],
        immature: None,
        l3: None,
    };
    (b.0.apply)(plan, &cx);
}

/// The model's answer for a class 7 case's last step in `mode`: the scenario's declared
/// occurrence there (parent state, no byte of the block) with, for the pair, the header rows
/// its last block measures (all block-level; none for the scenario solo).
pub fn scenario_answer(
    model: &Model,
    mode: Mode,
    par: Par,
    declared: &str,
    header: &[&str],
) -> precedence::Answer {
    let mut defects = vec![(declared, At::default())];
    defects.extend(header.iter().map(|r| (*r, At::default())));
    precedence::predicted(model, mode, par, &defects, &|_| {})
}

/// The model's answer for a block measured alone (its own setting).
pub fn predict_alone(model: &Model, mode: Mode, par: Par, m: &Measured) -> precedence::Answer {
    let (instances, facts) = setting_of(model, mode, par, m, &m.facts)?;
    Ok(precedence::walked(model, mode, par, &instances, &facts))
}

/// A composed pair's instances with their parts' footprints (footprint.rs: the shared fields a
/// variant sets, the guards it touches), for §4.3's rules: each part's on its first instance
/// (the part whose solo holds it, its row at its transaction). A footprint is a variant's, so
/// one part's several instances never meet a rule among themselves. A same-transaction pair's
/// two defects are one transaction's, built together: its instances carry none.
pub fn with_footprints(
    instances: &mut [Instance],
    pair: &Pair,
    m: &Measures,
) -> Result<(), String> {
    let parts = match pair.build {
        Build::Compose {
            a,
            b,
            ..
        }
        | Build::Scenario {
            a,
            b,
        } => [a, b],
        Build::SameTx(_) => return Ok(()),
    };
    let mut seen = [false; 2];
    for i in instances.iter_mut() {
        let vtx = i.trigger.iter().find(|(d, _)| d == "block.vtx").map(|(_, n)| *n);
        let holds = |k: usize| {
            m.solos[k]
                .defects
                .iter()
                .any(|(r, at)| *r == i.row && vtx.is_none_or(|v| at.vtx == Some(v)))
        };
        let k = (0..2).find(|k| holds(*k)).ok_or(format!("{}: {} is no part's", pair.id, i.row))?;
        if std::mem::replace(&mut seen[k], true) {
            continue;
        }
        let f = footprint::footprint(parts[k].0, parts[k].1).ok_or("a part with no footprint")?;
        i.footprint = f.fields.iter().map(|x| (*x).to_owned()).collect();
        i.touches = f.touches.iter().map(|x| (*x).to_owned()).collect();
    }
    Ok(())
}

/// The modes a pair case runs ([`Kind::Cold`] and [`Kind::Prop`]; a solo, cold only).
///
/// [`Kind::Cold`]: super::trial::Kind::Cold
/// [`Kind::Prop`]: super::trial::Kind::Prop
pub const MODES: [Mode; 2] = [Mode::Submit, Mode::Proposal];
pub const PARS: [Par; 2] = [Par::One, Par::Four];

/// The site a part runs at in a pair: its first site among Reg, BO and P1.
pub fn part_site(p: Part) -> Option<Site> {
    [Site::Reg, Site::BO, Site::P1].into_iter().find(|s| p.1.sites.contains(s))
}

/// The universe's parts (§4.5): per row, the first catalogue variant whose cold submit at its
/// site Core rejects there (a10-5's bound occurrence), outside group M, the scenarios, the
/// coinbase-less blocks and the reused setup (a10's declared txid reuse).
pub fn universe_parts() -> Vec<(&'static str, Part, Site)> {
    let mut out: Vec<(&'static str, Part, Site)> = Vec::new();
    for op in catalogue() {
        if op.name.starts_with("m_") || REUSE_SETUP.contains(&op.name) {
            continue;
        }
        for v in op.variants {
            let Some(site) = part_site((op, v)) else {
                continue;
            };
            let Some(f) = footprint::footprint(op, v) else {
                continue;
            };
            if scenario_steps(op.name, v.name).is_some()
                || f.fields.iter().any(|x| [CHAIN, BLOCK_TXS, COIN_REUSE].contains(x))
            {
                continue;
            }
            if let Expect::Reject {
                occurrence,
                ..
            } = (v.expect)(Mode::Submit, Par::One, site)
                && !out.iter().any(|(r, ..)| *r == occurrence)
            {
                out.push((occurrence, (op, v), site));
            }
        }
    }
    out
}

/// A synthetic chain for the dry run at `site`: the leg's clock and heights, the case's coins
/// funded at the split, the mints in the budget window, the coinbases by height.
pub struct Dry {
    pub height: u32,
    pub ledger: Ledger,
    pub template: BlockPlan,
    pub coins: Vec<Coin>,
    pub mints: Vec<Coin>,
    pub immature: Coin,
    pub mature: Coin,
    pub reward: i64,
}

const DRY_REWARD: i64 = 50_000_000_000;

impl Dry {
    pub fn new(site: Site, locks: &[Lock], mints: usize) -> Dry {
        let height = match site {
            Site::P1 => 115,
            Site::BO => 1_170,
            _ => 300,
        };
        let t0 = super::leg::T0 as u32;
        let mut ledger = Ledger::default();
        for h in 0..height {
            ledger.times.insert(h, t0 + 150 * h);
        }
        let coin = |tag: u8, n: u32, value: i64, lock: Lock, h: u32, cb: bool| Coin {
            outpoint: ([tag; 32], n),
            value,
            lock,
            height: h,
            coinbase: cb,
        };
        let coins: Vec<Coin> = locks
            .iter()
            .enumerate()
            .map(|(n, l)| coin(0x11, n as u32, 1_000_000_000, *l, 101, false))
            .collect();
        let mints: Vec<Coin> = (0..mints)
            .map(|n| coin(0x22, n as u32, MAX_MONEY, Lock::True, 1_000 + n as u32, true))
            .collect();
        let immature = coin(0x33, 0, DRY_REWARD, Lock::True, height - 99, true);
        let mature = coin(0x34, 0, DRY_REWARD, Lock::True, height - 100, true);
        for c in coins.iter().chain(&mints).chain([&immature, &mature]) {
            ledger
                .coins
                .insert(c.outpoint, (c.value, c.lock.script_pubkey(), c.height, c.coinbase));
        }
        let template = BlockPlan {
            prev: [0x44; 32],
            version: 0x2000_0000,
            time: t0 + 150 * height,
            bits: build::REGTEST_BITS,
            txs: vec![build::coinbase(height, DRY_REWARD, 0)],
            merkle_override: None,
        };
        Dry {
            height,
            ledger,
            template,
            coins,
            mints,
            immature,
            mature,
            reward: DRY_REWARD,
        }
    }

    pub fn world(&self, trial: [u8; 8]) -> World<'_> {
        World {
            template: &self.template,
            height: self.height,
            mtp: self.ledger.mtp(self.height - 1).unwrap_or(0),
            mocktime: i64::from(self.template.time),
            reward: self.reward,
            sb_limit: 0,
            coins: &self.coins,
            mints: &self.mints,
            immature: self.immature,
            mature: self.mature,
            spent: ([0x55; 32], 0),
            reuse: &[],
            coin_mtp: self.ledger.mtp(100).unwrap_or(0),
            trial,
        }
    }

    pub fn place(&self) -> Place<'_> {
        Place {
            height: self.height,
            mtp: self.ledger.mtp(self.height - 1).unwrap_or(0),
            now: i64::from(self.template.time),
            bits: self.template.bits,
            reward: self.reward,
            coins: &self.ledger,
        }
    }
}

/// The a10 binder's facts for a block (§3.5 step 3), as a leg's `execute` builds them.
pub fn bind_facts(plan: &BlockPlan, place: &Place<'_>, par: Par) -> BindFacts {
    let built = plan.build();
    BindFacts {
        network: "regtest".into(),
        dip3: false,
        cbtx: false,
        mnsync_synced: false,
        par,
        parent: ParentState::Clean,
        measured: super::ledger::measure(plan, &built.txids, place.height, place.mtp, place.coins),
    }
}

/// The a10 binder's binding of `want`'s reject reason for a block in `mode`, under the block's
/// [`bind_facts`]; none for an accepted block. Only an ambiguous one (a reason two rows share
/// under the block's measured facts, §3.5 unchanged) excludes a pair; an unbound reason or
/// another row is a harness error (Opus r1 m1).
pub fn binding(
    inventory: &Inventory,
    want: &Option<(String, bool)>,
    plan: &BlockPlan,
    facts: &BindFacts,
    mode: Mode,
) -> Option<Binding> {
    let (row, _) = want.as_ref()?;
    let probe = match row.as_str() {
        measure::CIS_SCRIPT => "mandatory-script-verify-flag-failed (Script evaluated without error but finished with a false/empty top stack element)".to_owned(),
        r if r.ends_with("bad-version(0x%08x)") => format!("bad-version(0x{:08x})", plan.version),
        r => reason(r).to_owned(),
    };
    Some(inventory.bind(&probe, mode, facts, None))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every universe part, alone on its dry chain, measures the occurrence a10-5 expects on
    /// every mode and `-par` its leg runs (the model over the measured instances), so the
    /// measurement reads Core's rules as a10-5's live expectations do.
    #[test]
    fn every_part_alone_measures_its_expectation() {
        let model = Model::load().expect("the model");
        let mut failed = Vec::new();
        for (row, part, site) in universe_parts() {
            let dry = Dry::new(site, &[Lock::P2shTrue; 9], part_mints(part, site));
            let world = dry.world(*b"o10-dry\0");
            let coins = world.part_coins(0, [part, part], site);
            let mut plan = world.applied(part, &coins, site);
            tag_coinbase(&mut plan, &world.trial, exact(part));
            let place = dry.place();
            let m = match measure::measure(&plan, &plan.build(), &place) {
                Ok(m) => m,
                Err(e) => {
                    failed.push(format!("{}: {e}", part_id(part)));
                    continue;
                }
            };
            for mode in MODES {
                for par in PARS {
                    let want = match (part.1.expect)(mode, par, site) {
                        Expect::Reject {
                            occurrence,
                            ..
                        } => Some(occurrence.to_owned()),
                        Expect::Accept => None,
                        other => {
                            failed.push(format!("{}: {other:?}", part_id(part)));
                            continue;
                        }
                    };
                    let defects: Vec<(&str, At)> =
                        m.defects.iter().map(|(r, a)| (r.as_str(), *a)).collect();
                    let got =
                        precedence::setting(&model, mode, par, &defects, Facts::default(), &|f| {
                            f.iter.extend(m.facts.iter.clone());
                            f.facts.extend(m.facts.facts.clone());
                            f.lengths.extend(m.facts.lengths.clone());
                        })
                        .map(|(i, f)| precedence::walked(&model, mode, par, &i, &f).0.map(|x| x.0));
                    if got.as_ref().ok() != Some(&want) {
                        failed.push(format!(
                            "{} ({row}) on {mode:?} at {par:?}: measured {:?}, model {got:?}, a10 {want:?}",
                            part_id(part),
                            m.defects
                        ));
                    }
                }
            }
        }
        assert!(failed.is_empty(), "{failed:#?}");
    }

    fn part(id: &str) -> Part {
        footprint::variants()
            .find(|(o, v)| footprint::variant_id(o, v) == id)
            .unwrap_or_else(|| panic!("no catalogue variant {id}"))
    }

    /// A composed pair measures exactly its solos' instances together, in both orders, each
    /// part's transactions where the order puts them; a block that loses a defect, or a solo
    /// that changes the transaction count, is refused as a broken composition.
    #[test]
    fn a_pair_is_its_solos_together() {
        let (a, b) = (part("bip68_height.reg"), part("in_below_out.reg"));
        for (order, at_a, at_b) in [(Order::Ab, 1, 2), (Order::Ba, 2, 1)] {
            let pair = Pair {
                id: "test",
                build: Build::Compose {
                    a,
                    b,
                    order,
                    fill: 0,
                },
                site: Site::Reg,
                class: 0,
                witness: None,
                mutants: &[],
                solos: ["", ""],
            };
            let dry = Dry::new(Site::Reg, &pair.locks(), pair.mints());
            let world = dry.world(*b"o10-test");
            let place = dry.place();
            let mut blocks = build(&pair, &world).expect("the blocks");
            let m = measures(&blocks, &place).expect("a sound composition");
            let rows = |m: &Measured| m.defects.clone();
            assert_eq!(rows(&m.solos[0]), [(measure::CONN_BIP68.to_owned(), at(at_a))]);
            assert_eq!(rows(&m.solos[1]), [(measure::CTI_BELOW.to_owned(), at(at_b))]);
            assert_eq!(m.pair.rows().len(), 2);
            let lost = Blocks {
                pair: blocks.solos[0].clone(),
                ..blocks.clone()
            };
            assert!(matches!(measures(&lost, &place), Err(MeasureError::Composition(_))));
            // A solo with one more (clean) transaction keeps its defect but not the setting.
            let extra = world.clean(1, 1);
            blocks.solos[1].txs.extend(extra);
            assert!(
                measures(&blocks, &place)
                    .is_err_and(|e| e.to_string().contains("transaction count"))
            );
        }
    }

    /// S-F1: a solo is the pair's block with one defect undone, every transaction keeping its
    /// input and output counts: a same-transaction negative shape keeps the negative output's
    /// place, a removed part's clean twins copy its transactions' counts (an empty domain, the
    /// defect itself, takes one). A solo that keeps its defects and the transaction count but
    /// shrinks an input or output domain is refused; each shrunk control is first checked to
    /// keep both, so the domain check alone refuses it.
    #[test]
    fn a_solo_keeps_the_pairs_domains() {
        let compose = |a: &str, b: &str| Pair {
            id: "test",
            build: Build::Compose {
                a: part(a),
                b: part(b),
                order: Order::Ab,
                fill: 0,
            },
            site: Site::Reg,
            class: 0,
            witness: None,
            mutants: &[],
            solos: ["", ""],
        };
        let same = |s: SameTx| Pair {
            id: "test",
            build: Build::SameTx(s),
            site: s.site(),
            class: s.class(),
            witness: None,
            mutants: &[],
            solos: ["", ""],
        };
        // (pair, a solo, the transaction to shrink, how)
        type Shrink = fn(&mut TxPlan);
        let drop_output_0: Shrink = |t| drop(t.outputs.remove(0));
        let drop_last_input: Shrink = |t| drop(t.inputs.pop());
        let drop_last_output: Shrink = |t| drop(t.outputs.pop());
        // One input fewer, its value out lowered so no input value goes missing.
        let drop_an_input: Shrink = |t| {
            t.inputs.pop();
            t.outputs[0].0 = 1;
        };
        let cases: [(Pair, usize, usize, Shrink); 4] = [
            // The below-out solo without the negative output's place.
            (same(SameTx::NegativeBelowOut), 1, 1, drop_output_0),
            // W3's range solo without its third (mature) input: the range still fails at j = 1.
            (same(SameTx::W3), 0, 1, drop_last_input),
            // The duplicate-input solo with bip68's clean twin one output short.
            (compose("duplicate_input.reg", "bip68_height.reg"), 0, 2, drop_last_output),
            // The bip68 solo with the duplicate input's two-input twin one input short.
            (compose("duplicate_input.reg", "bip68_height.reg"), 1, 1, drop_an_input),
        ];
        for (pair, k, i, shrink) in cases {
            let dry = Dry::new(pair.site, &pair.locks(), pair.mints());
            let world = dry.world(*b"o10-test");
            let place = dry.place();
            let blocks = build(&pair, &world).expect("the blocks");
            let m = measures(&blocks, &place).expect("a sound composition");
            for s in &m.solos {
                assert_eq!(s.shapes, m.pair.shapes, "{k} {i}");
            }
            let mut shrunk = blocks.clone();
            shrink(&mut shrunk.solos[k].txs[i]);
            let alone = measure::measure(&shrunk.solos[k], &shrunk.solos[k].build(), &place)
                .expect("the shrunk solo measures");
            assert_eq!(alone.defects, m.solos[k].defects, "the control keeps its defects");
            assert_eq!(alone.shapes.len(), m.solos[k].shapes.len(), "and the transaction count");
            assert_ne!(alone.shapes[i], m.pair.shapes[i], "but not the domain");
            let Err(e @ MeasureError::Composition(_)) = measures(&shrunk, &place) else {
                panic!("a shrunk domain is refused");
            };
            let e = e.to_string();
            assert!(e.contains(&format!("solo {k} changes tx {i}'s inputs and outputs")), "{e}");
        }
        // An emptied domain is the defect: vin_empty's twin takes one input.
        let pair = compose("vin_empty.reg", "bip68_height.reg");
        let dry = Dry::new(pair.site, &pair.locks(), pair.mints());
        let m =
            measures(&build(&pair, &dry.world(*b"o10-test")).expect("the blocks"), &dry.place())
                .expect("a sound composition");
        assert_eq!((m.pair.shapes[1], m.solos[1].shapes[1]), ((0, 2), (1, 2)));
    }

    /// Sol r1 S-F2: a part that funds its own spend (`script_false`'s fund and failing spend)
    /// has that coin locked at the split, and its block spends it directly: the script fault is
    /// the part's first transaction, at index 1 in front (W4) and the last index behind.
    #[test]
    fn a_prefunded_part_places_its_spend_first() {
        let script = part("script_false.reg");
        assert_eq!(prefund(script, Site::Reg), Some((1, Lock::False)));
        assert_eq!(prefund(part("bip68_height.reg"), Site::Reg), None);
        let other = part("bip68_height.reg");
        for (order, at_script, at_other) in [(Order::Ab, 1, 2), (Order::Ba, 2, 1)] {
            let pair = Pair {
                id: "test",
                build: Build::Compose {
                    a: script,
                    b: other,
                    order,
                    fill: 0,
                },
                site: Site::Reg,
                class: 0,
                witness: None,
                mutants: &[],
                solos: ["", ""],
            };
            assert_eq!(pair.locks()[1], Lock::False);
            let dry = Dry::new(Site::Reg, &pair.locks(), pair.mints());
            let blocks = build(&pair, &dry.world(*b"o10-test")).expect("the blocks");
            assert_eq!(blocks.pair.txs.len(), 3, "no funding transaction");
            let m = measures(&blocks, &dry.place()).expect("a sound composition");
            let script_at = At {
                vtx: Some(at_script),
                vin: Some(0),
                vout: None,
            };
            assert_eq!(m.solos[0].defects, [(measure::CIS_SCRIPT.to_owned(), script_at)]);
            assert_eq!(m.solos[1].defects, [(measure::CONN_BIP68.to_owned(), at(at_other))]);
        }
    }

    /// S-F3: a CheckTransaction loop row's measured input or output index stands in the pair
    /// and in its solo alike (`vout_negative`'s output 1, `duplicate_input`'s repeated input 1).
    #[test]
    fn a_solo_keeps_its_loop_trigger() {
        let b = part("bip68_height.reg");
        for (a, row, trigger) in [
            (
                part("vout_negative.reg"),
                measure::CHECK_TRANSACTION[4],
                At {
                    vtx: Some(1),
                    vin: None,
                    vout: Some(1),
                },
            ),
            (
                part("duplicate_input.reg"),
                measure::CHECK_TRANSACTION[7],
                At {
                    vtx: Some(1),
                    vin: Some(1),
                    vout: None,
                },
            ),
        ] {
            let pair = Pair {
                id: "test",
                build: Build::Compose {
                    a,
                    b,
                    order: Order::Ab,
                    fill: 0,
                },
                site: Site::Reg,
                class: 0,
                witness: None,
                mutants: &[],
                solos: ["", ""],
            };
            let dry = Dry::new(Site::Reg, &pair.locks(), pair.mints());
            let world = dry.world(*b"o10-test");
            let m = measures(&build(&pair, &world).expect("the blocks"), &dry.place())
                .expect("a sound composition");
            let want = (row.to_owned(), trigger);
            assert_eq!(m.solos[0].defects, std::slice::from_ref(&want), "{row}");
            assert!(m.pair.defects.contains(&want), "{row}: {:?}", m.pair.defects);
        }
    }

    fn at(vtx: usize) -> At {
        At {
            vtx: Some(vtx),
            vin: None,
            vout: None,
        }
    }
}
