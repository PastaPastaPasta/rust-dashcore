//! O10-3's generator (O10 plan §3.4): seeded field mutations of a valid base block, each checked
//! on the final bytes, run on both sides with dashd's reply as the reference (differential
//! credit only).
//!
//! A case `n` of a run seeded `seed` is built in three parts, each from its own stream of
//! `(seed, n)`, so nothing about it depends on `--only`, the shard or the filter (a10 rev1 §3.2):
//! - its **pool slots** ([`slots`]): the three coins the setup's split lays down for it at
//!   [`SPLIT_HEIGHT`](super::leg::SPLIT_HEIGHT), slots `3n..3n+2` of the run's pool;
//! - its **population** ([`population`]): `k ∈ [1, 6]` ordinary transactions spending its
//!   spendable slots, with spend chains (probability ½ that a transaction spends its
//!   predecessor's output), after dashd's coinbase-only template;
//! - its **draws** ([`pick`]): the mutations of sub-seed `(seed, n, r)`, `r < 4`, each checked
//!   on the final bytes ([`check`]).

use std::sync::LazyLock;

use super::ledger::is_coinbase;
use super::trial::to_hex;
use crate::chain::build::{
    BlockPlan, Built, Input, Lock, OP_0, OP_RETURN, REGTEST_BITS, SIGOPS_MAX, TxPlan,
    legacy_sigops, push, script_num,
};
use crate::chain::mutate::{
    BUDGET_START, Carrier, Expect, MAX_MONEY, Operator, Site, Stage, Variant,
};
use crate::chain::world::Coin;

/// A run's generator settings (§3.4, §8.2): its seed, its case count `N` per `-par`, and the
/// shards each `-par`'s cases are split over (one leg each).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    pub seed: u64,
    pub n: usize,
    pub shards: u32,
}

/// The nightly's settings (§8.2: N = 2,000 per `-par`, 4 shards per `-par`, OD-L3-40); the
/// seed is the run's, printed in its report.
pub const DEFAULT: Config = Config {
    seed: 0x0010_0003,
    n: 2_000,
    shards: 4,
};

/// Generated cases per height (an explicit per-site budget, lesson 4): every case at a height
/// builds on the same tip from the same template, its blocks its own by the trial's carrier
/// and coinbase tag.
pub const PER_HEIGHT: u32 = 4;
/// The first generated case's height: after the twins, their controls and the pinned case
/// (201-205, Reg).
pub const FIRST_HEIGHT: u32 = super::leg::CASE_BASE + 5;
/// The most cases a run fits on Reg below the budget heights (1000): over it, the run refuses
/// to start (the budget is L3's to change, never sampled silently).
pub const MAX_CASES: usize = ((BUDGET_START - FIRST_HEIGHT) * PER_HEIGHT) as usize;

impl Config {
    /// `L3_GEN_SEED` (decimal or 0x-hex), `L3_GEN_N` and `L3_GEN_SHARDS`, each defaulting to
    /// [`DEFAULT`]'s.
    pub fn from_env() -> Result<Config, String> {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        let number = |name: &str, text: String| {
            match text.strip_prefix("0x") {
                Some(hex) => u64::from_str_radix(hex, 16),
                None => text.parse(),
            }
            .map_err(|e| format!("{name}={text}: {e}"))
        };
        let seed = var("L3_GEN_SEED").map_or(Ok(DEFAULT.seed), |t| number("L3_GEN_SEED", t))?;
        let n = var("L3_GEN_N").map_or(Ok(DEFAULT.n as u64), |t| number("L3_GEN_N", t))? as usize;
        let shards = var("L3_GEN_SHARDS")
            .map_or(Ok(u64::from(DEFAULT.shards)), |t| number("L3_GEN_SHARDS", t))?;
        let config = Config {
            seed,
            n,
            shards: u32::try_from(shards).map_err(|e| format!("L3_GEN_SHARDS: {e}"))?,
        };
        config.check()?;
        Ok(config)
    }

    pub fn check(&self) -> Result<(), String> {
        if self.n == 0 || self.n > MAX_CASES {
            return Err(format!(
                "L3_GEN_N={}: the Reg heights {FIRST_HEIGHT}..{BUDGET_START} fit 1..={MAX_CASES} \
                 generated cases at {PER_HEIGHT} per height",
                self.n
            ));
        }
        // Each shard is a leg per `-par`, with its own dashd and V: past one shard per case of a
        // height a leg runs no case at most heights and only repeats setup (lesson 4: budgets
        // explicit; OD-L3-65 n5).
        if self.shards == 0 || self.shards > PER_HEIGHT {
            return Err(format!("L3_GEN_SHARDS={}: 1..={PER_HEIGHT} shards", self.shards));
        }
        // The regime filter reads the case's height, not a `hdr.prev` draw's measured one
        // (OD-L3-65 n2): sound only while every case is below DIP3, so a set past it (M4,
        // o10_dip3) needs the measured height first.
        let last = self.height(self.n - 1);
        if last >= DIP3_HEIGHT {
            return Err(format!(
                "L3_GEN_N={}: case height {last} reaches DIP3 ({DIP3_HEIGHT}); the regime filter \
                 reads the case's height, so a generated set at or past DIP3 (M4, o10_dip3) needs \
                 the block's measured height first",
                self.n
            ));
        }
        Ok(())
    }

    /// Case `n`'s height.
    pub fn height(&self, n: usize) -> u32 {
        FIRST_HEIGHT + n as u32 / PER_HEIGHT
    }

    /// Case `n`'s shard.
    pub fn shard(&self, n: usize) -> u32 {
        n as u32 % self.shards
    }
}

/// The process's settings, read once: every leg, the report and the census read the same run.
pub fn config() -> &'static Result<Config, String> {
    static CONFIG: LazyLock<Result<Config, String>> = LazyLock::new(Config::from_env);
    &CONFIG
}

/// A generated case: case `n` of the run seeded `seed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Generated {
    pub seed: u64,
    pub n: usize,
}

/// The cases' variant names, `<seed>.<n>` (ids `o10.gen.<seed>.<n>@Reg/<par>`, OD-L3-13), for
/// the process's run.
pub fn variants() -> &'static [String] {
    static NAMES: LazyLock<Vec<String>> = LazyLock::new(|| match config() {
        Ok(c) => (0..c.n).map(|n| format!("{:x}.{n}", c.seed)).collect(),
        Err(_) => Vec::new(),
    });
    &NAMES
}

/// The operator every generated case names (its id `o10.gen`): its block is the generator's
/// and its answer dashd's (differential credit only, §3.4), never this operator's.
pub static GEN_OPERATOR: Operator = Operator {
    name: "o10.gen",
    rejects: &[],
    stage: Stage::Header,
    par: false,
    apply: |_, _| {},
    variants: &[Variant {
        name: "gen",
        sites: &[Site::Reg],
        expect: |_, _, _| Expect::Accept,
        carrier: Carrier::OpReturn,
        full_only: false,
    }],
};

/// A deterministic stream (SplitMix64): the generator's only source of choices.
#[derive(Clone, Debug)]
pub struct Rng(u64);

/// SplitMix64's finalizer.
fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

impl Rng {
    /// The stream of `parts` (seed, stream name, case, draw, ...).
    pub fn new(parts: &[u64]) -> Rng {
        Rng(parts.iter().fold(0x6f31_302d_3367_656e, |s, p| mix(s ^ p)))
    }

    pub fn word(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        mix(self.0)
    }

    /// A number in `0..n` (`n` > 0).
    pub fn below(&mut self, n: u64) -> u64 {
        self.word() % n
    }

    /// True `num` times in `den`.
    pub fn chance(&mut self, num: u64, den: u64) -> bool {
        self.below(den) < num
    }

    pub fn pick<T: Copy>(&mut self, xs: &[T]) -> T {
        xs[self.below(xs.len() as u64) as usize]
    }

    pub fn bytes32(&mut self) -> [u8; 32] {
        let mut out = [0u8; 32];
        for chunk in out.chunks_mut(8) {
            chunk.copy_from_slice(&self.word().to_le_bytes());
        }
        out
    }
}

/// The streams of a case.
const POOL: u64 = 0x706f_6f6c;
const POPULATION: u64 = 0x706f_7075;

/// The slots each case has in the run's pool (§3.4: `cases × 3`).
pub const SLOTS: usize = 3;
/// The CLTV slots' lock height: below every generated case's height, at or past CLTV's
/// activation (140), so the base's spend is final and its lock met.
pub const CLTV_LOCK: u32 = 190;
/// The deepest relative lock a slot carries: the pool's last split height plus this is at most
/// the first generated case's height ([`FIRST_HEIGHT`]), so every slot is deep enough
/// (BIP68's height lock: the coin's height + r ≤ the block's).
pub const RELATIVE_MAX: u8 = 90;
/// Cases per pool split transaction (three outputs each), as a10's split.
pub const SPLIT_CASES: usize = 300;
/// The pool's first split height (§3.4: laid down at fixed heights): after a10's split (101),
/// its `T_base′` (102) and that one's spend (103). Chunk `k` of the generated cases is laid
/// down at `POOL_HEIGHT + k`, spending the coinbase at height `POOL_COINBASE + k`.
pub const POOL_HEIGHT: u32 = super::leg::SPLIT_HEIGHT + 3;
/// The pool's first coinbase: after a10's split's one (1) and `T_base′`'s (2); mature at its
/// split height (`POOL_COINBASE + k + 100 ≤ POOL_HEIGHT + k`).
pub const POOL_COINBASE: u32 = 3;
const _: () = assert!(POOL_COINBASE + crate::chain::build::MATURITY <= POOL_HEIGHT);
/// The pool's split heights for `n` cases.
pub fn pool_heights(n: usize) -> std::ops::Range<u32> {
    POOL_HEIGHT..POOL_HEIGHT + n.div_ceil(SPLIT_CASES) as u32
}
const _: () = assert!(CLTV_LOCK < FIRST_HEIGHT && CLTV_LOCK >= crate::chain::mutate::CLTV_HEIGHT);
/// What each population transaction leaves its block.
pub const FEE: i64 = 1_000;

/// A pool slot: the lock the split puts its coin under, and how a valid spend of it looks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    /// `OP_TRUE`.
    True,
    /// A P2SH of `OP_TRUE` (standard: dashd's mempool takes a spend back at a rollback).
    P2shTrue,
    /// `<CLTV_LOCK> CHECKLOCKTIMEVERIFY`: spent with that lock time and a non-final sequence.
    Cltv,
    /// `<r> CHECKSEQUENCEVERIFY`: spent with sequence `r` (BIP68 and the opcode both hold).
    Csv(u8),
    /// `OP_TRUE` spent with BIP68 sequence `r`: a relative lock of `r` blocks, met.
    Bip68(u8),
    /// The sigop filler `Lock::Sigops(k)`: a spend with `k` accurate sigops.
    Sigops(u8),
    /// A lock puzzle no key opens on the regtest regime (a10 rev1 §4's notes): never spent by a
    /// base, only by a `tx.prevout` mutation (a script failure).
    Puzzle(Lock),
}

/// The puzzles a slot may hold.
pub const PUZZLES: [Lock; 4] = [Lock::P2pkhBadSig, Lock::P2shBadSig, Lock::NonDerNot, Lock::False];

impl Slot {
    pub fn lock(self) -> Lock {
        match self {
            Slot::True | Slot::Bip68(_) => Lock::True,
            Slot::P2shTrue => Lock::P2shTrue,
            Slot::Cltv => Lock::Cltv(CLTV_LOCK),
            Slot::Csv(r) => Lock::Csv(u32::from(r)),
            Slot::Sigops(k) => Lock::Sigops(k),
            Slot::Puzzle(lock) => lock,
        }
    }

    pub fn spendable(self) -> bool {
        !matches!(self, Slot::Puzzle(_))
    }

    /// The sequence of a valid spend.
    pub fn sequence(self) -> u32 {
        match self {
            Slot::Cltv => 0xffff_fffe,
            Slot::Csv(r) | Slot::Bip68(r) => u32::from(r),
            _ => u32::MAX,
        }
    }

    /// The lock time a valid spend needs (0: none).
    pub fn lock_time(self) -> u32 {
        match self {
            Slot::Cltv => CLTV_LOCK,
            _ => 0,
        }
    }

    pub fn name(self) -> String {
        match self {
            Slot::True => "true".into(),
            Slot::P2shTrue => "p2sh_true".into(),
            Slot::Cltv => "cltv".into(),
            Slot::Csv(r) => format!("csv{r}"),
            Slot::Bip68(r) => format!("bip68_{r}"),
            Slot::Sigops(k) => format!("sigops{k}"),
            Slot::Puzzle(lock) => format!("puzzle_{lock:?}"),
        }
    }
}

/// Case `n`'s pool slots: slot 0 always spendable, the others possibly puzzles.
pub fn slots(seed: u64, n: usize) -> [Slot; SLOTS] {
    let mut rng = Rng::new(&[seed, POOL, n as u64]);
    std::array::from_fn(|i| {
        let kinds = if i == 0 {
            9
        } else {
            10
        };
        match rng.below(kinds) {
            0 | 1 => Slot::True,
            2..=4 => Slot::P2shTrue,
            5 => Slot::Cltv,
            6 => Slot::Csv(1 + rng.below(u64::from(RELATIVE_MAX)) as u8),
            7 => Slot::Bip68(1 + rng.below(u64::from(RELATIVE_MAX)) as u8),
            8 => Slot::Sigops(1 + rng.below(u64::from(SIGOPS_MAX)) as u8),
            _ => Slot::Puzzle(rng.pick(&PUZZLES)),
        }
    })
}

/// The coinbase's origin; a population transaction's is its index from 1; [`INSERTED`] and up
/// are a draw's own transactions.
pub const COINBASE_ORIGIN: u32 = 0;
pub const INSERTED: u32 = 1_000;

/// A population transaction: its plan, its origin (a stable name across reordering) and, per
/// input, the in-block output it spends (`(origin, vout)`), which finalization re-links to that
/// transaction's final txid (OD-L3-13: dependent spends are rebuilt per trial).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenTx {
    pub origin: u32,
    pub tx: TxPlan,
    pub links: Vec<Option<(u32, u32)>>,
    /// What its inputs spend, in satoshis.
    pub value_in: i64,
}

/// The trial's carrier: an `OP_RETURN` output pushing its id, last in every transaction the
/// generator builds, so each trial's transactions are its own (OD-L3-13).
pub fn carrier(trial: &[u8; 8]) -> (i64, Vec<u8>) {
    (0, [vec![OP_RETURN], push(trial)].concat())
}

/// Case `n`'s population on its coins (its three slots, in slot order): `k ∈ [1, 6]`
/// transactions, each spending a free spendable slot or, with probability ½ (always once the
/// slots are spent), an output of its predecessor; a quarter of them take a second input. Each
/// has one to three value outputs and the trial's carrier. `Err` if the coins are not the slots'.
pub fn population(
    seed: u64,
    n: usize,
    coins: &[Coin],
    trial: &[u8; 8],
) -> Result<Vec<GenTx>, String> {
    let kinds = slots(seed, n);
    if coins.len() != SLOTS || coins.iter().zip(kinds).any(|(c, k)| c.lock != k.lock()) {
        return Err(format!("case {n}: its coins are not its pool slots {kinds:?}"));
    }
    let mut rng = Rng::new(&[seed, POPULATION, n as u64]);
    let k = 1 + rng.below(6) as usize;
    let mut free: Vec<usize> = (0..SLOTS).filter(|&s| kinds[s].spendable()).collect();
    // Unspent value outputs: (tx index, vout, value, lock).
    let mut unspent: Vec<(usize, u32, i64, Lock)> = Vec::new();
    let mut txs: Vec<GenTx> = Vec::new();
    for i in 0..k {
        let from_last: Vec<usize> =
            (0..unspent.len()).filter(|&u| i > 0 && unspent[u].0 == i - 1).collect();
        let chain = !from_last.is_empty() && (free.is_empty() || rng.chance(1, 2));
        let mut inputs = Vec::new();
        let mut links = Vec::new();
        let (mut value_in, mut lock_time) = (0i64, 0u32);
        let mut take_slot =
            |s: usize, inputs: &mut Vec<Input>, links: &mut Vec<Option<(u32, u32)>>| {
                let coin = coins[s];
                let mut input = Input::new(coin.outpoint, coin.lock.script_sig());
                input.sequence = kinds[s].sequence();
                lock_time = lock_time.max(kinds[s].lock_time());
                inputs.push(input);
                links.push(None);
                coin.value
            };
        let take_output = |u: (usize, u32, i64, Lock),
                           txs: &[GenTx],
                           inputs: &mut Vec<Input>,
                           links: &mut Vec<Option<(u32, u32)>>| {
            let parent = &txs[u.0];
            inputs.push(Input::new((parent.tx.txid(), u.1), u.3.script_sig()));
            links.push(Some((parent.origin, u.1)));
            u.2
        };
        if chain {
            let u = unspent.remove(from_last[rng.below(from_last.len() as u64) as usize]);
            value_in += take_output(u, &txs, &mut inputs, &mut links);
        } else if !free.is_empty() {
            let s = free.remove(0);
            value_in += take_slot(s, &mut inputs, &mut links);
        } else {
            break;
        }
        if rng.chance(1, 4) {
            if !free.is_empty() {
                let s = free.remove(0);
                value_in += take_slot(s, &mut inputs, &mut links);
            } else if !unspent.is_empty() {
                let u = unspent.remove(rng.below(unspent.len() as u64) as usize);
                value_in += take_output(u, &txs, &mut inputs, &mut links);
            }
        }
        let outs = 1 + rng.below(3) as usize;
        let each = (value_in - FEE) / outs as i64;
        let locks: Vec<Lock> = (0..outs)
            .map(|_| {
                if rng.chance(3, 4) {
                    Lock::P2shTrue
                } else {
                    Lock::True
                }
            })
            .collect();
        let mut tx =
            TxPlan::simple(inputs, locks.iter().map(|l| (each, l.script_pubkey())).collect());
        tx.lock_time = lock_time;
        tx.outputs.push(carrier(trial));
        for (vout, lock) in locks.iter().enumerate() {
            unspent.push((i, vout as u32, each, *lock));
        }
        txs.push(GenTx {
            origin: i as u32 + 1,
            tx,
            links,
            value_in,
        });
    }
    Ok(txs)
}

// ---- Commit 3: the draws (mutations, finalization, no-ops) ----

/// The mutation families (§3.4), in the census's order (`GEN_FAMILIES`), each drawn with equal
/// weight.
pub const FAMILIES: [&str; 15] = [
    "hdr.version",
    "hdr.prev",
    "hdr.merkle",
    "hdr.time",
    "hdr.bits",
    "cb.scriptsig",
    "cb.value",
    "cb.outputs",
    "tx.version",
    "tx.locktime",
    "tx.sequence",
    "tx.prevout",
    "tx.value",
    "tx.script",
    "blk.structure",
];

/// Re-draws before a case is recorded as a no-op (§3.4: sub-seeds `r < 4`).
pub const DRAWS: u32 = 4;
const DRAW: u64 = 0x6472_6177;

/// How finalization sets the header's merkle root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Merkle {
    /// The transactions' root (re-rooted after every mutation).
    Honest,
    /// A `hdr.merkle` mutation's value (`merkle_override`).
    Fixed([u8; 32]),
    /// A `blk.structure` duplicate's: the root of the list without the duplicate (origin), so
    /// the header keeps the root the block had before it (CVE-2012-2459's shape when the
    /// duplicate ends an odd list).
    BeforeDuplicate(u32),
}

/// A block being drawn: its plan, each transaction's origin and in-block links, how its merkle
/// root and proof of work are finalized.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Draft {
    pub plan: BlockPlan,
    pub origin: Vec<u32>,
    pub links: Vec<Vec<Option<(u32, u32)>>>,
    /// What each transaction's inputs spend (0 where unknown: the coinbase, inserted ones).
    pub value_in: Vec<i64>,
    pub merkle: Merkle,
    /// Grind the proof of work against the header's final `nBits` (§3.4 step 4); false for a
    /// `hdr.bits` value no grind meets ("leave unground": high-hash is the expected class).
    pub grind: bool,
}

impl Draft {
    /// The base: dashd's coinbase-only template (its coinbase tagged) and the population.
    pub fn base(template: &BlockPlan, population: &[GenTx]) -> Draft {
        let mut plan = template.clone();
        plan.merkle_override = None;
        plan.txs.truncate(1);
        plan.txs.extend(population.iter().map(|t| t.tx.clone()));
        Draft {
            plan,
            origin: std::iter::once(COINBASE_ORIGIN)
                .chain(population.iter().map(|t| t.origin))
                .collect(),
            links: std::iter::once(vec![None; template.txs[0].inputs.len()])
                .chain(population.iter().map(|t| t.links.clone()))
                .collect(),
            value_in: std::iter::once(0).chain(population.iter().map(|t| t.value_in)).collect(),
            merkle: Merkle::Honest,
            grind: true,
        }
    }

    /// The index of the first transaction of `origin`.
    pub fn index(&self, origin: u32) -> Option<usize> {
        self.origin.iter().position(|o| *o == origin)
    }

    /// Whether the transaction of origin `o` is `on` or spends it through a chain of links.
    pub fn depends_on(&self, o: u32, on: u32) -> bool {
        let mut stack = vec![o];
        let mut seen = Vec::new();
        while let Some(x) = stack.pop() {
            if x == on {
                return true;
            }
            if seen.contains(&x) {
                continue;
            }
            seen.push(x);
            for (k, origin) in self.origin.iter().enumerate() {
                if *origin == x {
                    stack.extend(self.links[k].iter().flatten().map(|(p, _)| *p));
                }
            }
        }
        false
    }

    /// The final block (§3.4's order): every link re-pointed at its parent's final txid, then
    /// the merkle root (honest unless a mutation set it), then the proof of work against the
    /// header's own `nBits` (or none, unground). `Err` for a link cycle or a grind that fails.
    pub fn finish(&self) -> Result<(BlockPlan, Built), String> {
        let n = self.plan.txs.len();
        if [self.origin.len(), self.links.len(), self.value_in.len()] != [n; 3] {
            return Err(format!(
                "the draft's {n} transactions have {} origins, {} link lists, {} input values",
                self.origin.len(),
                self.links.len(),
                self.value_in.len()
            ));
        }
        let mut plan = self.plan.clone();
        relink(&mut plan.txs, &self.origin, &self.links)?;
        plan.merkle_override = match self.merkle {
            Merkle::Honest => None,
            Merkle::Fixed(root) => Some(root),
            Merkle::BeforeDuplicate(dup) => {
                let mut without = plan.clone();
                if let Some(i) = self.index(dup) {
                    without.txs.remove(i);
                }
                Some(merkle_root(&without))
            }
        };
        let built = plan.finish(self.grind)?;
        Ok((plan, built))
    }
}

/// Every link re-pointed at its parent's final txid (a parent by origin, the first of it; a link
/// to an origin the block lacks keeps its prevout). Each pass settles every transaction whose
/// linked parents are settled, so `n` passes settle an acyclic block; `Err` for a cycle.
fn relink(
    txs: &mut [TxPlan],
    origin: &[u32],
    links: &[Vec<Option<(u32, u32)>>],
) -> Result<(), String> {
    let n = txs.len();
    if links.len() != n || links.iter().zip(txs.iter()).any(|(l, t)| l.len() != t.inputs.len()) {
        return Err("the links do not cover the transactions' inputs".into());
    }
    let index = |o: u32| origin.iter().position(|x| *x == o);
    let mut done = vec![false; n];
    for _ in 0..=n {
        let mut progress = false;
        for i in 0..n {
            if done[i] {
                continue;
            }
            let ready =
                links[i].iter().flatten().all(|(o, _)| index(*o).is_none_or(|p| done[p] && p != i));
            if !ready {
                continue;
            }
            for (j, link) in links[i].iter().enumerate() {
                if let Some((o, vout)) = link
                    && let Some(p) = index(*o)
                {
                    txs[i].inputs[j].prevout = (txs[p].txid(), *vout);
                }
            }
            done[i] = true;
            progress = true;
        }
        if !progress {
            break;
        }
    }
    if done.iter().any(|d| !d) {
        return Err("a link cycle in the draft".into());
    }
    Ok(())
}

/// The transactions' merkle root of `plan` (its header's at `REGTEST_BITS`).
pub fn merkle_root(plan: &BlockPlan) -> [u8; 32] {
    let honest = BlockPlan {
        merkle_override: None,
        bits: REGTEST_BITS,
        ..plan.clone()
    };
    honest.build().header[36..68].try_into().expect("32 bytes")
}

/// What a draw sets on the case's chain (§3.4's mutation values that come from the leg).
#[derive(Clone, Debug)]
pub struct DrawCx {
    pub height: u32,
    /// The parent's median time past and the leg's clock.
    pub mtp: u32,
    pub now: i64,
    /// The tip's parent and the genesis block: known blocks a `hdr.prev` can name.
    pub grandparent: [u8; 32],
    pub genesis: [u8; 32],
    /// Coins a `tx.prevout` can name: the one setup spent, an immature coinbase, the case's
    /// puzzle slots.
    pub spent: ([u8; 32], u32),
    pub immature: ([u8; 32], u32),
    pub puzzles: Vec<([u8; 32], u32)>,
    /// The case's own pool slots, every one (the outpoints a `vout + 1` may name, OD-L3-65 n6).
    pub slots: Vec<([u8; 32], u32)>,
    pub trial: [u8; 8],
}

/// Where a mutation lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Target {
    Header,
    /// The transaction of this origin (the coinbase's is [`COINBASE_ORIGIN`]).
    Tx(u32),
    Block,
}

/// The field a mutation sets, which the changed-field check reads from the final bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Field {
    Version,
    Prev,
    Merkle,
    Time,
    Bits,
    /// A transaction's 32-bit version (version and type).
    TxVersion,
    LockTime,
    ScriptSig(usize),
    Sequence(usize),
    Prevout(usize),
    Value(usize),
    ScriptPubKey(usize),
    /// The coinbase's whole output list.
    Outputs,
    /// `blk.structure`: the origin is at this index (insert, with what it spends; a duplicate's
    /// source, its copy and the copy's index), gone (delete), the two swapped, or the origin
    /// moved to this index.
    Inserted(u32, usize, Insert),
    Deleted(u32),
    Duplicated(u32, u32, usize),
    Swapped(u32, u32),
    Moved(u32, usize),
}

/// An inserted transaction's kind, all of its content the record does not fix by shape
/// (OD-L3-65 n4): it is one input and the outputs `[(1000, P2SH true), the trial's carrier]`,
/// and its input is a second coinbase's (null prevout, scriptSig `height ‖ push(word)`), a
/// spend of an unknown outpoint `(txid, 0)` (empty scriptSig), or a spend of output 0 of the
/// in-block transaction of this origin (P2SH true's scriptSig). [`check`] rebuilds the
/// transaction from this and the carrier alone, never from the final bytes.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Insert {
    Coinbase {
        height: u32,
        word: u64,
    },
    Unknown([u8; 32]),
    Spend(u32),
}

impl Insert {
    /// The inserted transaction for the trial's `carrier` and its link.
    fn tx(self, carrier: (i64, Vec<u8>)) -> (TxPlan, Option<(u32, u32)>) {
        let (input, link) = match self {
            Insert::Coinbase {
                height,
                word,
            } => {
                let script = [script_num(i64::from(height)), push(&word.to_le_bytes())].concat();
                (Input::new(([0; 32], u32::MAX), script), None)
            }
            Insert::Unknown(txid) => (Input::new((txid, 0), Vec::new()), None),
            // Its prevout is the parent's final txid once relinked.
            Insert::Spend(parent) => {
                (Input::new(([0; 32], 0), Lock::P2shTrue.script_sig()), Some((parent, 0)))
            }
        };
        let tx =
            TxPlan::simple(vec![input], vec![(1_000, Lock::P2shTrue.script_pubkey()), carrier]);
        (tx, link)
    }
}

/// Short, as the report's field labels read it.
impl std::fmt::Debug for Insert {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Insert::Coinbase {
                height,
                word,
            } => write!(f, "Coinbase({height}, {word:#x})"),
            Insert::Unknown(txid) => write!(f, "Unknown({})", to_hex(txid)),
            Insert::Spend(parent) => write!(f, "Spend({parent})"),
        }
    }
}

/// A field's value: bytes, or a prevout naming an in-block output by origin (resolved against
/// the final block).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Val {
    Bytes(Vec<u8>),
    Link(u32, u32),
    /// A structural mutation: the check is the field's own relation.
    Structure,
}

/// One recorded mutation (§3.4 step 2: `(family, target, field, old, new)`; `old` is read from
/// the base's final bytes by [`check`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mutation {
    pub family: &'static str,
    pub target: Target,
    pub field: Field,
    pub new: Val,
}

/// A draw of sub-seed `(seed, n, r)` on a base.
#[derive(Clone, Debug)]
pub struct Draw {
    pub r: u32,
    /// The families drawn, in draw order (one per mutation slot), and the mutations applied.
    pub drawn: Vec<&'static str>,
    pub mutations: Vec<Mutation>,
    pub draft: Draft,
}

/// The regime filter (§3.4): no special version ≥ 3 with `nType` ≠ 0, no type-5 coinbase. It
/// reads a block decoded from its final bytes ([`pick`], OD-L3-53), and a coinbase is one by
/// `IsCoinBase`'s predicate ([`is_coinbase`]), wherever it stands (OD-L3-54).
///
/// Below DIP3's activation it also refuses a non-coinbase tx with `nType == 6` and
/// `nVersion < 3` ([`QC_TYPE_PRE_DIP3`], DEC-1059, OD-L3-45). Core connects such a block (v24.0.0
/// rc.5 `CQuorumBlockProcessor::ProcessBlock` returns before reading the block below DIP3,
/// llmq/blockprocessor.cpp:218), but its undo reads every `nType == 6` tx as a commitment without
/// checking the version (`UndoBlock` → `GetCommitmentsFromBlock`, :526, :573), fails, and the
/// disconnect fails: the leg's rollback of an accepted cold submit cannot run. Exactly that
/// selector; the pinned proposal-only case `o10.qc_type_pre_dip3` keeps Core's verdict on it.
///
/// It skips a coinbase, though Core's hazard does not: `GetCommitmentsFromBlock` reads every
/// `nType == 6` tx, the coinbase too (rc.5 llmq/blockprocessor.cpp:572-573). That is safe because
/// a generated type-6 coinbase never connects (OD-L3-65 n1,
/// `a_generated_type_6_coinbase_never_connects`):
/// - **Not at vtx\[0\]:** CheckBlock refuses it first, `bad-cb-missing` or `bad-cb-multiple`
///   (rc.5 validation.cpp:4409, :4412).
/// - **At vtx\[0\]:** it needs all three of a draw's mutations: `tx.version` (version < 3,
///   type 6) on a population tx, `tx.prevout` nulling its single input, and `blk.structure`
///   taking the template coinbase off vtx\[0\]. An inserted coinbase is never a `tx.*` target.
///   No mutation is left for its scriptSig, which is a pool slot's or a chain output's. Each
///   such scriptSig fails `bad-cb-length` (2..=100 bytes, rc.5 consensus/tx_check.cpp:66-67) or
///   BIP34's `bad-cb-height` (it must begin with `CScript() << nHeight`; rc.5
///   validation.cpp:4592-4596, BIP34Height = 1 on regtest, chainparams.cpp:816), all before
///   connect. That holds at every height a generated block has: the case heights, one below
///   them, and 1 (a `hdr.prev` of the tip's parent or of genesis). It is height-bound: P2SH
///   true's scriptSig `01 51` is `CScript() << 81`.
///
/// A family that writes a coinbase's scriptSig together with `tx.version`, or a fourth mutation
/// per draw, voids this: the selector would then have to read the coinbase too.
pub fn regime_refusal(plan: &BlockPlan, height: u32) -> Option<String> {
    if let Some(i) =
        plan.txs.iter().position(|t| is_coinbase(t) && t.tx_type == TRANSACTION_COINBASE)
    {
        return Some(format!("tx {i}: a type-5 coinbase"));
    }
    if let Some(i) = plan.txs.iter().position(|t| t.version >= 3 && t.tx_type != 0) {
        return Some(format!("tx {i}: version ≥ 3 with nType ≠ 0"));
    }
    if height < DIP3_HEIGHT
        && let Some(i) = plan.txs.iter().position(|t| {
            !is_coinbase(t) && t.tx_type == TRANSACTION_QUORUM_COMMITMENT && t.version < 3
        })
    {
        return Some(format!("{QC_TYPE_PRE_DIP3}: tx {i}"));
    }
    None
}

/// The narrowed filter's reason (DEC-1059): a pre-DIP3 non-coinbase `nType == 6`, `nVersion < 3`.
pub const QC_TYPE_PRE_DIP3: &str = "regime:qc-type-pre-dip3";
/// `TRANSACTION_QUORUM_COMMITMENT`'s type (primitives/transaction.h:35).
pub const TRANSACTION_QUORUM_COMMITMENT: u16 = 6;
/// The census arguments' DIP3 activation height (`-dip3params=2000:2000`).
pub const DIP3_HEIGHT: u32 = super::live::DIP3_HEIGHT;

/// `CbTx`'s type: a coinbase of this type is evo territory (O10-5, O10-7a).
pub const TRANSACTION_COINBASE: u16 = 5;

/// One to three mutations of sub-seed `(seed, n, r)` on `base`, in draw order (the regime filter
/// reads the final block, in [`pick`]). A family with no candidate left (every value equal to the field's, no target, a second
/// structural or merkle mutation) is drawn and skips.
pub fn draw(seed: u64, n: usize, r: u32, base: &Draft, cx: &DrawCx) -> Draw {
    let mut rng = Rng::new(&[seed, DRAW, n as u64, u64::from(r)]);
    let count = match rng.below(10) {
        0..=4 => 1,
        5..=7 => 2,
        _ => 3,
    };
    let mut d = base.clone();
    let mut drawn = Vec::new();
    let mut mutations: Vec<Mutation> = Vec::new();
    for _ in 0..count {
        let family = rng.pick(&FAMILIES);
        drawn.push(family);
        if let Some(m) = mutate(family, &mut d, &mut rng, cx, &mutations) {
            mutations.push(m);
        }
    }
    Draw {
        r,
        drawn,
        mutations,
        draft: d,
    }
}

/// Pick among `options` the ones not equal to `current`.
fn fresh<T: Clone + PartialEq>(rng: &mut Rng, options: &[T], current: &T) -> Option<T> {
    let left: Vec<&T> = options.iter().filter(|o| *o != current).collect();
    (!left.is_empty()).then(|| left[rng.below(left.len() as u64) as usize].clone())
}

/// The ordinary transactions (never a coinbase, by its predicate wherever it stands: an inserted
/// one included, OD-L3-54): their origins.
fn ordinary(d: &Draft) -> Vec<u32> {
    d.origin.iter().zip(&d.plan.txs).filter(|(_, t)| !is_coinbase(t)).map(|(o, _)| *o).collect()
}

/// The value outputs of a transaction: every output but a trailing carrier.
fn value_outputs(d: &Draft, i: usize, cx: &DrawCx) -> usize {
    let outs = &d.plan.txs[i].outputs;
    outs.len() - usize::from(outs.last() == Some(&carrier(&cx.trial)))
}

/// The length of a script's first operation (a push and its data, or one opcode).
fn first_op(script: &[u8]) -> usize {
    match script.first() {
        None => 0,
        Some(&n @ 1..=75) => 1 + usize::from(n),
        Some(0x4c) => 2 + script.get(1).map_or(0, |n| usize::from(*n)),
        Some(0x4d) => {
            3 + script.get(1..3).map_or(0, |n| usize::from(u16::from_le_bytes([n[0], n[1]])))
        }
        Some(_) => 1,
    }
    .min(script.len())
}

fn le<T: Into<i64>>(x: T) -> Vec<u8> {
    x.into().to_le_bytes().to_vec()
}

/// One mutation of `family` on `d`: its record, or `None` where the family has no candidate.
/// No two mutations of a draw set the same field, and a draw has at most one structural and one
/// merkle-setting mutation.
fn mutate(
    family: &'static str,
    d: &mut Draft,
    rng: &mut Rng,
    cx: &DrawCx,
    earlier: &[Mutation],
) -> Option<Mutation> {
    let taken = |target: Target, field: Field| {
        earlier.iter().any(|m| m.target == target && m.field == field)
    };
    let record = |target, field, new| {
        Some(Mutation {
            family,
            target,
            field,
            new,
        })
    };
    let header = |field| (!taken(Target::Header, field)).then_some(());
    match family {
        "hdr.version" => {
            header(Field::Version)?;
            let options =
                [1, 2, 3, 4, 0x2000_0000, 0x2000_0001, 0x2000_0010, 0x3000_0000, -1, i32::MAX];
            let v = fresh(rng, &options, &d.plan.version)?;
            d.plan.version = v;
            record(Target::Header, Field::Version, Val::Bytes(v.to_le_bytes().to_vec()))
        }
        "hdr.prev" => {
            header(Field::Prev)?;
            let options = [rng.bytes32(), cx.grandparent, cx.genesis];
            let v = fresh(rng, &options, &d.plan.prev)?;
            d.plan.prev = v;
            record(Target::Header, Field::Prev, Val::Bytes(v.to_vec()))
        }
        "hdr.merkle" => {
            if d.merkle != Merkle::Honest {
                return None;
            }
            let v = if rng.chance(1, 4) {
                [0; 32]
            } else {
                rng.bytes32()
            };
            d.merkle = Merkle::Fixed(v);
            record(Target::Header, Field::Merkle, Val::Bytes(v.to_vec()))
        }
        "hdr.time" => {
            header(Field::Time)?;
            let now = u32::try_from(cx.now).unwrap_or(u32::MAX);
            let edge = u32::try_from(cx.now + crate::chain::build::MAX_FUTURE).unwrap_or(u32::MAX);
            let t = d.plan.time;
            let options = [
                cx.mtp,
                cx.mtp.saturating_add(1),
                cx.mtp.saturating_sub(1),
                edge,
                edge.saturating_add(1),
                now,
                t.saturating_sub(1),
                t.saturating_add(1),
                0,
                u32::MAX,
            ];
            let v = fresh(rng, &options, &t)?;
            d.plan.time = v;
            record(Target::Header, Field::Time, Val::Bytes(v.to_le_bytes().to_vec()))
        }
        "hdr.bits" => {
            header(Field::Bits)?;
            let options = [
                REGTEST_BITS - 1,
                0x207f_0000,
                0x2080_0000,
                0x217f_ffff,
                0x1d00_ffff,
                0x2000_ffff,
                0,
                rng.word() as u32,
            ];
            let v = fresh(rng, &options, &d.plan.bits)?;
            d.plan.bits = v;
            d.grind = grindable(v);
            record(Target::Header, Field::Bits, Val::Bytes(v.to_le_bytes().to_vec()))
        }
        "cb.scriptsig" => {
            let target = Target::Tx(COINBASE_ORIGIN);
            (!taken(target, Field::ScriptSig(0))).then_some(())?;
            let c = d.index(COINBASE_ORIGIN)?;
            let s = d.plan.txs[c].inputs.first()?.script_sig.clone();
            let rest = s[first_op(&s)..].to_vec();
            let h = i64::from(cx.height);
            let options = [
                [script_num(h + 1), rest.clone()].concat(),
                [script_num(h + 2), rest.clone()].concat(),
                [script_num(0), rest.clone()].concat(),
                s[..1.min(s.len())].to_vec(),
                [s.clone(), vec![OP_0; 101usize.saturating_sub(s.len())]].concat(),
                [s.clone(), push(&rng.word().to_le_bytes()[..4])].concat(),
            ];
            let v = fresh(rng, &options, &s)?;
            d.plan.txs[c].inputs[0].script_sig = v.clone();
            record(target, Field::ScriptSig(0), Val::Bytes(v))
        }
        "cb.value" => {
            let target = Target::Tx(COINBASE_ORIGIN);
            let c = d.index(COINBASE_ORIGIN)?;
            let outs = &d.plan.txs[c].outputs;
            if outs.is_empty() || taken(target, Field::Value(0)) || taken(target, Field::Outputs) {
                return None;
            }
            let v = outs[0].0;
            let options = [v + 1, v - 1, 0, -1, MAX_MONEY + 1, 2 * v];
            let new = fresh(rng, &options, &v)?;
            d.plan.txs[c].outputs[0].0 = new;
            record(target, Field::Value(0), Val::Bytes(le(new)))
        }
        "cb.outputs" => {
            let target = Target::Tx(COINBASE_ORIGIN);
            if taken(target, Field::Outputs) || taken(target, Field::Value(0)) {
                return None;
            }
            let c = d.index(COINBASE_ORIGIN)?;
            let outs = d.plan.txs[c].outputs.clone();
            let mut split = outs.clone();
            if let Some(first) = split.first_mut() {
                let half = first.0 / 2;
                first.0 -= half;
                split.push((half, Lock::P2shTrue.script_pubkey()));
            }
            let mut sigops = outs.clone();
            if let Some(first) = sigops.first_mut() {
                first.1 = legacy_sigops(1_000);
            }
            let options = [
                [
                    outs.clone(),
                    vec![(0, [vec![OP_RETURN], push(&rng.word().to_le_bytes())].concat())],
                ]
                .concat(),
                split,
                sigops,
                Vec::new(),
            ];
            let v = fresh(rng, &options, &outs)?;
            d.plan.txs[c].outputs = v.clone();
            record(target, Field::Outputs, Val::Bytes(outputs_bytes(&v)))
        }
        "tx.version" | "tx.locktime" | "tx.sequence" | "tx.prevout" | "tx.value" | "tx.script" => {
            // A duplicate pair stays a duplicate: an earlier `Duplicated` freezes both copies.
            let frozen: Vec<u32> = earlier
                .iter()
                .flat_map(|m| match m.field {
                    Field::Duplicated(src, dup, _) => vec![src, dup],
                    _ => Vec::new(),
                })
                .collect();
            let targets: Vec<u32> =
                ordinary(d).into_iter().filter(|o| !frozen.contains(o)).collect();
            if targets.is_empty() {
                return None;
            }
            let origin = rng.pick(&targets);
            let i = d.index(origin).expect("a present origin");
            let target = Target::Tx(origin);
            let tx = d.plan.txs[i].clone();
            match family {
                "tx.version" => {
                    (!taken(target, Field::TxVersion)).then_some(())?;
                    let version = rng.pick(&[0i16, 1, 2, 3, 4, 0x7fff]);
                    let tx_type = if rng.chance(4, 5) {
                        0
                    } else {
                        1 + rng.below(9) as u16
                    };
                    let v = fresh(rng, &[(version, tx_type)], &(tx.version, tx.tx_type))?;
                    (d.plan.txs[i].version, d.plan.txs[i].tx_type) = v;
                    record(target, Field::TxVersion, Val::Bytes(tx_version(v.0, v.1)))
                }
                "tx.locktime" => {
                    (!taken(target, Field::LockTime)).then_some(())?;
                    let now = u32::try_from(cx.now).unwrap_or(u32::MAX);
                    let options = [
                        cx.height - 1,
                        cx.height,
                        cx.height + 1,
                        CLTV_LOCK - 1,
                        cx.mtp - 1,
                        cx.mtp,
                        now,
                        u32::MAX,
                        0,
                    ];
                    let v = fresh(rng, &options, &tx.lock_time)?;
                    d.plan.txs[i].lock_time = v;
                    record(target, Field::LockTime, Val::Bytes(v.to_le_bytes().to_vec()))
                }
                "tx.sequence" => {
                    let j = rng.below(tx.inputs.len() as u64) as usize;
                    (!taken(target, Field::Sequence(j))).then_some(())?;
                    let options = [
                        0,
                        0xffff_fffe,
                        u32::MAX,
                        1 + rng.below(120) as u32,
                        1 << 22 | (1 + rng.below(100) as u32),
                        1 << 31 | rng.below(1 << 16) as u32,
                    ];
                    let v = fresh(rng, &options, &tx.inputs[j].sequence)?;
                    d.plan.txs[i].inputs[j].sequence = v;
                    record(target, Field::Sequence(j), Val::Bytes(v.to_le_bytes().to_vec()))
                }
                "tx.prevout" => {
                    let j = rng.below(tx.inputs.len() as u64) as usize;
                    (!taken(target, Field::Prevout(j))).then_some(())?;
                    let current = match d.links[i][j] {
                        Some((o, v)) => Val::Link(o, v),
                        None => Val::Bytes(outpoint_bytes(tx.inputs[j].prevout)),
                    };
                    let mut options = vec![
                        Val::Bytes(outpoint_bytes((rng.bytes32(), 0))),
                        Val::Bytes(outpoint_bytes(cx.spent)),
                        Val::Bytes(outpoint_bytes(cx.immature)),
                        Val::Link(COINBASE_ORIGIN, 0),
                        Val::Bytes(outpoint_bytes(([0; 32], u32::MAX))),
                    ];
                    // The spent output's next one, only where the case owns it (OD-L3-65 n6):
                    // an in-block parent's, or another of the case's own pool slots. The slots
                    // are laid down consecutively, so a last slot's next is the next case's.
                    let next = (tx.inputs[j].prevout.0, tx.inputs[j].prevout.1.wrapping_add(1));
                    if d.links[i][j].is_some() || cx.slots.contains(&next) {
                        options.push(Val::Bytes(outpoint_bytes(next)));
                    }
                    options.extend(cx.puzzles.iter().map(|p| Val::Bytes(outpoint_bytes(*p))));
                    // Another input's spend: of the same transaction (a duplicate input) or of
                    // another (an in-block double spend).
                    for (k, other) in d.plan.txs.iter().enumerate().skip(1) {
                        for (l, input) in other.inputs.iter().enumerate() {
                            if (k, l) != (i, j) {
                                options.push(match d.links[k][l] {
                                    Some((o, v)) => Val::Link(o, v),
                                    None => Val::Bytes(outpoint_bytes(input.prevout)),
                                });
                            }
                        }
                    }
                    // A link to the transaction itself or to a descendant of it is a cycle no
                    // transaction can hold (its txid would commit to itself): not an option.
                    let own = d.origin[i];
                    options.retain(|o| !matches!(o, Val::Link(p, _) if d.depends_on(*p, own)));
                    // A link names a transaction of the block: another input's link to one an
                    // earlier `blk.structure` delete removed is no option (its record could never
                    // hold in the final bytes; fix round 1, found by the 10^5 draws checked).
                    options.retain(|o| !matches!(o, Val::Link(p, _) if d.index(*p).is_none()));
                    let v = fresh(rng, &options, &current)?;
                    match &v {
                        Val::Link(o, vout) => {
                            d.links[i][j] = Some((*o, *vout));
                            if let Some(p) = d.index(*o) {
                                d.plan.txs[i].inputs[j].prevout = (d.plan.txs[p].txid(), *vout);
                            }
                        }
                        Val::Bytes(b) => {
                            d.links[i][j] = None;
                            d.plan.txs[i].inputs[j].prevout = outpoint_of(b);
                        }
                        Val::Structure => unreachable!("a prevout is bytes or a link"),
                    }
                    record(target, Field::Prevout(j), v)
                }
                "tx.value" => {
                    let outs = value_outputs(d, i, cx);
                    if outs == 0 {
                        return None;
                    }
                    let k = rng.below(outs as u64) as usize;
                    (!taken(target, Field::Value(k))).then_some(())?;
                    let v = tx.outputs[k].0;
                    let others: i64 = tx
                        .outputs
                        .iter()
                        .enumerate()
                        .filter(|(x, _)| *x != k)
                        .map(|(_, o)| o.0)
                        .sum();
                    let over = d.value_in[i] - others + 1;
                    let options = [-1, 0, MAX_MONEY, MAX_MONEY + 1, over, v - 1, v + 1];
                    let new = fresh(rng, &options, &v)?;
                    d.plan.txs[i].outputs[k].0 = new;
                    record(target, Field::Value(k), Val::Bytes(le(new)))
                }
                _ => {
                    // tx.script: an input's scriptSig or an output's scriptPubKey.
                    if rng.chance(1, 2) {
                        let j = rng.below(tx.inputs.len() as u64) as usize;
                        (!taken(target, Field::ScriptSig(j))).then_some(())?;
                        let options = [
                            Vec::new(),
                            vec![OP_0],
                            push(&rng.word().to_le_bytes()[..3]),
                            vec![0x61],
                        ];
                        let v = fresh(rng, &options, &tx.inputs[j].script_sig)?;
                        d.plan.txs[i].inputs[j].script_sig = v.clone();
                        record(target, Field::ScriptSig(j), Val::Bytes(v))
                    } else {
                        let outs = value_outputs(d, i, cx);
                        if outs == 0 {
                            return None;
                        }
                        let k = rng.below(outs as u64) as usize;
                        (!taken(target, Field::ScriptPubKey(k))).then_some(())?;
                        let options = [
                            Vec::new(),
                            vec![OP_RETURN],
                            vec![OP_0],
                            legacy_sigops(1_000),
                            legacy_sigops(20_001),
                            legacy_sigops(40_001),
                        ];
                        let v = fresh(rng, &options, &tx.outputs[k].1)?;
                        d.plan.txs[i].outputs[k].1 = v.clone();
                        record(target, Field::ScriptPubKey(k), Val::Bytes(v))
                    }
                }
            }
        }
        _ => structure(d, rng, cx, earlier),
    }
}

/// `blk.structure`: insert, delete, duplicate, swap or move (one per draw).
fn structure(d: &mut Draft, rng: &mut Rng, cx: &DrawCx, earlier: &[Mutation]) -> Option<Mutation> {
    if earlier.iter().any(|m| m.family == "blk.structure") {
        return None;
    }
    let family = "blk.structure";
    let len = d.plan.txs.len();
    let record = |field| {
        Some(Mutation {
            family,
            target: Target::Block,
            field,
            new: Val::Structure,
        })
    };
    // An index, the coinbase's (0) one time in eight.
    let index = |rng: &mut Rng| {
        if rng.chance(1, 8) {
            0
        } else {
            1 + rng.below((len - 1).max(1) as u64) as usize
        }
    };
    let new_origin = INSERTED + d.origin.iter().filter(|o| **o >= INSERTED).count() as u32;
    match rng.below(5) {
        0 => {
            // Insert a transaction of the trial's own: a second coinbase, a spend of an unknown
            // outpoint, or a spend of a population output.
            let kind = match rng.below(3) {
                0 => Insert::Coinbase {
                    height: cx.height,
                    word: rng.word(),
                },
                1 => Insert::Unknown(rng.bytes32()),
                _ => Insert::Spend(rng.pick(&d.origin)),
            };
            let (mut tx, link) = kind.tx(carrier(&cx.trial));
            if let Insert::Spend(parent) = kind {
                // The draft's prevout is the parent's txid now (finalization re-links it).
                let p = d.index(parent).expect("a present origin");
                tx.inputs[0].prevout = (d.plan.txs[p].txid(), 0);
            }
            let at = 1 + rng.below(len as u64) as usize;
            let at = if rng.chance(1, 8) {
                0
            } else {
                at.min(len)
            };
            d.plan.txs.insert(at, tx);
            d.origin.insert(at, new_origin);
            d.links.insert(at, vec![link]);
            d.value_in.insert(at, 0);
            record(Field::Inserted(new_origin, at, kind))
        }
        1 => {
            // Delete a transaction no earlier mutation of the draw names.
            // ... nor a link of one names (its check resolves the link in the final block).
            let named: Vec<u32> = earlier
                .iter()
                .flat_map(|m| {
                    let target = match m.target {
                        Target::Tx(o) => Some(o),
                        _ => None,
                    };
                    let link = match m.new {
                        Val::Link(o, _) => Some(o),
                        _ => None,
                    };
                    target.into_iter().chain(link)
                })
                .collect();
            let free: Vec<usize> = (0..len).filter(|&i| !named.contains(&d.origin[i])).collect();
            let free: Vec<usize> = if rng.chance(1, 8) {
                free
            } else {
                free.into_iter().filter(|i| *i > 0).collect()
            };
            if free.is_empty() {
                return None;
            }
            let i = rng.pick(&free);
            let origin = d.origin.remove(i);
            d.plan.txs.remove(i);
            d.links.remove(i);
            d.value_in.remove(i);
            record(Field::Deleted(origin))
        }
        2 => {
            if d.merkle != Merkle::Honest || len < 2 {
                return None;
            }
            let i = 1 + rng.below((len - 1) as u64) as usize;
            let at = if rng.chance(1, 2) {
                len
            } else {
                i + 1
            };
            let (tx, links, value) = (d.plan.txs[i].clone(), d.links[i].clone(), d.value_in[i]);
            let source = d.origin[i];
            d.plan.txs.insert(at, tx);
            d.origin.insert(at, new_origin);
            d.links.insert(at, links);
            d.value_in.insert(at, value);
            d.merkle = Merkle::BeforeDuplicate(new_origin);
            record(Field::Duplicated(source, new_origin, at))
        }
        3 => {
            if len < 2 {
                return None;
            }
            let (i, j) = (index(rng), index(rng));
            if i == j {
                return None;
            }
            d.plan.txs.swap(i, j);
            d.origin.swap(i, j);
            d.links.swap(i, j);
            d.value_in.swap(i, j);
            record(Field::Swapped(d.origin[j], d.origin[i]))
        }
        _ => {
            if len < 2 {
                return None;
            }
            let (i, to) = (index(rng), index(rng));
            if i == to {
                return None;
            }
            let (tx, origin, links, value) =
                (d.plan.txs.remove(i), d.origin.remove(i), d.links.remove(i), d.value_in.remove(i));
            d.plan.txs.insert(to, tx);
            d.origin.insert(to, origin);
            d.links.insert(to, links);
            d.value_in.insert(to, value);
            record(Field::Moved(origin, to))
        }
    }
}

/// A transaction's 32-bit version field (`nVersion | nType << 16`).
pub fn tx_version(version: i16, tx_type: u16) -> Vec<u8> {
    ((version as u16 as u32) | (u32::from(tx_type) << 16)).to_le_bytes().to_vec()
}

pub fn outpoint_bytes(p: ([u8; 32], u32)) -> Vec<u8> {
    [p.0.to_vec(), p.1.to_le_bytes().to_vec()].concat()
}

fn outpoint_of(b: &[u8]) -> ([u8; 32], u32) {
    (
        b[..32].try_into().expect("32 bytes"),
        u32::from_le_bytes(b[32..36].try_into().expect("4 bytes")),
    )
}

/// An output list's bytes: each value then its script, length-prefixed.
pub fn outputs_bytes(outs: &[(i64, Vec<u8>)]) -> Vec<u8> {
    outs.iter()
        .flat_map(|(v, s)| {
            [v.to_le_bytes().to_vec(), (s.len() as u32).to_le_bytes().to_vec(), s.clone()].concat()
        })
        .collect()
}

/// Whether a grind can meet `bits`: Core's `CheckProofOfWork` takes the target (positive, not
/// overflowing, nonzero, no easier than regtest's powLimit) and a nonce meets it within about
/// 2^16 tries (target ≥ 2^240).
pub fn grindable(bits: u32) -> bool {
    let exponent = bits >> 24;
    let mantissa = bits & 0x007f_ffff;
    if mantissa == 0 || !crate::chain::build::meets_bits(&[0; 32], bits) {
        return false;
    }
    let log2 = i64::from(31 - mantissa.leading_zeros()) + 8 * (i64::from(exponent) - 3);
    log2 >= 240
}

/// A trial's block, drawn: the draw, the final block decoded from its bytes, the bytes, and each
/// mutation checked.
#[derive(Clone, Debug)]
pub struct Final {
    pub draw: Draw,
    /// Decoded from `built` ([`check`]), its header's merkle root as `merkle_override`: the
    /// plan the trial's facts read (OD-L3-53), not the draft's.
    pub plan: BlockPlan,
    pub built: Built,
    pub checked: Vec<Checked>,
}

/// A mutation checked on the final bytes: `(family, target, field, old, new)`, hex.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checked {
    pub family: &'static str,
    pub target: Target,
    pub field: Field,
    pub old: String,
    pub new: String,
}

/// A case's draws (§3.4's no-op handling): the first sub-seed `r < DRAWS` whose final block, for
/// every trial, differs from that trial's base, holds every recorded mutation, passes the regime
/// filter and has a hash neither side knows. Otherwise the case is a typed no-op or a filter
/// refusal.
#[derive(Clone, Debug)]
pub enum Outcome {
    /// One final block per trial, in the bases' order.
    Picked {
        r: u32,
        trials: Vec<Final>,
        tried: Vec<Tried>,
    },
    /// Every draw was a no-op (equal to its base, or a known hash) or refused, at least one a
    /// no-op.
    Noop {
        tried: Vec<Tried>,
    },
    /// Every draw was refused by the regime filter.
    Filtered {
        tried: Vec<Tried>,
    },
}

/// A draw passed over, with why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tried {
    pub r: u32,
    pub drawn: Vec<&'static str>,
    pub mutations: Vec<&'static str>,
    pub reason: String,
}

/// A trial's base: its draft and its final block.
pub struct Base {
    pub draft: Draft,
    pub built: Built,
    pub cx: DrawCx,
}

/// The case's outcome over its trials' bases: `known` says whether either side knows a hash.
/// `Err` is a harness error: a finalization, a decode or a changed-field check that fails.
pub fn pick(
    seed: u64,
    n: usize,
    bases: &[Base],
    known: impl FnMut(&[u8; 32]) -> Result<bool, String>,
) -> Result<Outcome, String> {
    pick_draws(bases, |r| bases.iter().map(|b| draw(seed, n, r, &b.draft, &b.cx)).collect(), known)
}

/// [`pick`] over the draws `next(r)` gives for each sub-seed, one per base (OD-L3-53's order):
/// every draw is finalized, then a draw equal to its base is a no-op, then every recorded
/// mutation is checked on the decoded final bytes, then the regime filter reads the decoded
/// block, then a known hash is a no-op. A finalization, decode or check that fails is a harness
/// error whatever the filter would say: it never becomes a refusal, a no-op or a redraw.
pub fn pick_draws(
    bases: &[Base],
    mut next: impl FnMut(u32) -> Vec<Draw>,
    mut known: impl FnMut(&[u8; 32]) -> Result<bool, String>,
) -> Result<Outcome, String> {
    let mut tried = Vec::new();
    for r in 0..DRAWS {
        let draws = next(r);
        if draws.len() != bases.len() {
            return Err(format!("draw {r}: {} draws for {} bases", draws.len(), bases.len()));
        }
        // §8.1's unit is a cold submit and its proposal twin: every trial's draw is the same
        // mutations, by family, target and field (OD-L3-65 n3), else the trials pair unlike
        // blocks.
        let shape = |d: &Draw| -> Vec<(&'static str, Target, Field)> {
            d.mutations.iter().map(|m| (m.family, m.target, m.field)).collect()
        };
        if let Some(k) = draws.iter().position(|d| shape(d) != shape(&draws[0])) {
            return Err(format!(
                "draw {r}: trial {k}'s mutations {:?} are not trial 0's {:?}",
                shape(&draws[k]),
                shape(&draws[0])
            ));
        }
        let summary = |reason: String| Tried {
            r,
            drawn: draws[0].drawn.clone(),
            mutations: draws[0].mutations.iter().map(|m| m.family).collect(),
            reason,
        };
        let mut finals = Vec::new();
        for (d, b) in draws.iter().zip(bases) {
            let (_, built) = d.draft.finish().map_err(|e| format!("draw {r}: {e}"))?;
            finals.push((d, built, b));
        }
        if finals.iter().any(|(d, built, b)| d.mutations.is_empty() || built.bytes == b.built.bytes)
        {
            tried.push(summary("noop: the base's bytes".into()));
            continue;
        }
        let mut trials = Vec::new();
        for (d, built, b) in finals {
            let (plan, checked) = check(d, &b.draft, &b.built, &built, &b.cx.trial)
                .map_err(|e| format!("draw {r}: {e}"))?;
            trials.push((
                Final {
                    draw: d.clone(),
                    plan,
                    built,
                    checked,
                },
                b.cx.height,
            ));
        }
        if let Some(refusal) = trials.iter().find_map(|(f, h)| regime_refusal(&f.plan, *h)) {
            tried.push(summary(format!("filtered: {refusal}")));
            continue;
        }
        let mut hit = None;
        for (f, _) in &trials {
            if known(&f.built.hash)? {
                hit = Some(f.built.hash);
                break;
            }
        }
        if let Some(hash) = hit {
            tried.push(summary(format!("noop: a known hash {}", super::trial::hex(&hash))));
            continue;
        }
        return Ok(Outcome::Picked {
            r,
            trials: trials.into_iter().map(|(f, _)| f).collect(),
            tried,
        });
    }
    if tried.iter().any(|t| t.reason.starts_with("noop")) {
        Ok(Outcome::Noop {
            tried,
        })
    } else {
        Ok(Outcome::Filtered {
            tried,
        })
    }
}

/// A decoded block and its transactions' origins: where a mutation's field is read.
struct Read<'a> {
    header: [u8; 80],
    plan: BlockPlan,
    origin: &'a [u32],
}

impl Read<'_> {
    fn new<'a>(built: &Built, origin: &'a [u32]) -> Result<Read<'a>, String> {
        let plan = BlockPlan::from_block(&built.bytes)?;
        if plan.txs.len() != origin.len() {
            return Err(format!("{} transactions decoded, {} drawn", plan.txs.len(), origin.len()));
        }
        Ok(Read {
            header: built.bytes[..80].try_into().expect("80 bytes"),
            plan,
            origin,
        })
    }

    fn index(&self, origin: u32) -> Option<usize> {
        self.origin.iter().position(|o| *o == origin)
    }

    /// The field's bytes at the target, `None` where the target is gone.
    fn field(&self, target: Target, field: Field) -> Option<Vec<u8>> {
        let h = &self.header;
        let tx = |o: u32| self.index(o).map(|i| &self.plan.txs[i]);
        Some(match (target, field) {
            (Target::Header, Field::Version) => h[0..4].to_vec(),
            (Target::Header, Field::Prev) => h[4..36].to_vec(),
            (Target::Header, Field::Merkle) => h[36..68].to_vec(),
            (Target::Header, Field::Time) => h[68..72].to_vec(),
            (Target::Header, Field::Bits) => h[72..76].to_vec(),
            (Target::Tx(o), Field::TxVersion) => tx(o).map(|t| tx_version(t.version, t.tx_type))?,
            (Target::Tx(o), Field::LockTime) => tx(o)?.lock_time.to_le_bytes().to_vec(),
            (Target::Tx(o), Field::ScriptSig(j)) => tx(o)?.inputs.get(j)?.script_sig.clone(),
            (Target::Tx(o), Field::Sequence(j)) => {
                tx(o)?.inputs.get(j)?.sequence.to_le_bytes().to_vec()
            }
            (Target::Tx(o), Field::Prevout(j)) => outpoint_bytes(tx(o)?.inputs.get(j)?.prevout),
            (Target::Tx(o), Field::Value(k)) => le(tx(o)?.outputs.get(k)?.0),
            (Target::Tx(o), Field::ScriptPubKey(k)) => tx(o)?.outputs.get(k)?.1.clone(),
            (Target::Tx(o), Field::Outputs) => outputs_bytes(&tx(o)?.outputs),
            _ => return None,
        })
    }

    /// A structural mutation's relation, against the base, on the measured origins.
    fn structure(&self, field: Field, base: &Read<'_>) -> Result<(), String> {
        let ok = match field {
            Field::Inserted(o, at, _) => self.index(o) == Some(at) && base.index(o).is_none(),
            Field::Deleted(o) => self.index(o).is_none() && base.index(o).is_some(),
            Field::Duplicated(src, dup, at) => match (self.index(src), self.index(dup)) {
                (Some(a), Some(b)) => {
                    b == at
                        && self.plan.txs[a].txid() == self.plan.txs[b].txid()
                        && base.index(dup).is_none()
                }
                _ => false,
            },
            Field::Swapped(a, b) => {
                self.index(a).is_some()
                    && self.index(a) == base.index(b)
                    && self.index(b) == base.index(a)
            }
            Field::Moved(o, to) => self.index(o) == Some(to) && base.index(o) != Some(to),
            other => return Err(format!("{other:?} is not structural")),
        };
        if !ok {
            return Err(format!(
                "blk.structure {field:?}: not in the final bytes (final {:?}, base {:?})",
                self.origin, base.origin
            ));
        }
        Ok(())
    }
}

/// The links a decoded block's inputs hold: an input whose prevout names another transaction of
/// the block spends it, by that transaction's origin.
fn measured_links(txs: &[TxPlan], origin: &[u32]) -> Vec<Vec<Option<(u32, u32)>>> {
    let ids: Vec<[u8; 32]> = txs.iter().map(TxPlan::txid).collect();
    txs.iter()
        .enumerate()
        .map(|(k, t)| {
            t.inputs
                .iter()
                .map(|i| {
                    ids.iter()
                        .position(|id| *id == i.prevout.0)
                        .filter(|p| *p != k)
                        .map(|p| (origin[p], i.prevout.1))
                })
                .collect()
        })
        .collect()
}

/// A draw's transactions rebuilt from the base's decoded ones and the draw's records alone
/// (OD-L3-52): never from the draft, so a recorded operation the draft did not make (or one it
/// made unrecorded) leaves the rebuilt list unlike the decoded final one.
struct Replay {
    origin: Vec<u32>,
    txs: Vec<TxPlan>,
    links: Vec<Vec<Option<(u32, u32)>>>,
    /// A duplicate's origin: the header keeps the root without it.
    dup: Option<u32>,
    /// The trial's carrier, an inserted transaction's last output.
    carrier: (i64, Vec<u8>),
}

impl Replay {
    fn index(&self, o: u32) -> Result<usize, String> {
        self.origin.iter().position(|x| *x == o).ok_or(format!("origin {o} is not in the block"))
    }

    /// One record applied: a transaction's field set to the recorded value, or the recorded
    /// structural operation (an inserted transaction built from its record's kind and link and
    /// the trial's carrier, OD-L3-65 n4: no transaction is taken from the final bytes).
    fn apply(&mut self, m: &Mutation) -> Result<(), String> {
        let bytes = |n: usize| match &m.new {
            Val::Bytes(b) if b.len() == n || n == 0 => Ok(b.clone()),
            other => Err(format!("{other:?}: not {n} bytes")),
        };
        let word = || bytes(4).map(|b| u32::from_le_bytes(b[..4].try_into().expect("4 bytes")));
        let input = |r: &mut Replay, i: usize, j: usize| {
            r.txs[i].inputs.get_mut(j).map(|_| ()).ok_or(format!("tx {i} has no input {j}"))
        };
        match (m.target, m.field) {
            (Target::Header, _) => {}
            (Target::Tx(o), field) => {
                let i = self.index(o)?;
                match field {
                    Field::TxVersion => {
                        let v = word()?;
                        self.txs[i].version = v as u16 as i16;
                        self.txs[i].tx_type = (v >> 16) as u16;
                    }
                    Field::LockTime => self.txs[i].lock_time = word()?,
                    Field::Sequence(j) => {
                        input(self, i, j)?;
                        self.txs[i].inputs[j].sequence = word()?;
                    }
                    Field::ScriptSig(j) => {
                        input(self, i, j)?;
                        self.txs[i].inputs[j].script_sig = bytes(0)?;
                    }
                    Field::Prevout(j) => {
                        input(self, i, j)?;
                        match &m.new {
                            Val::Bytes(b) if b.len() == 36 => {
                                self.txs[i].inputs[j].prevout = outpoint_of(b);
                                self.links[i][j] = None;
                            }
                            Val::Link(p, vout) => self.links[i][j] = Some((*p, *vout)),
                            other => return Err(format!("a prevout of {other:?}")),
                        }
                    }
                    Field::Value(k) => {
                        let v = i64::from_le_bytes(bytes(8)?[..].try_into().expect("8 bytes"));
                        self.txs[i]
                            .outputs
                            .get_mut(k)
                            .ok_or(format!("tx {i} has no output {k}"))?
                            .0 = v;
                    }
                    Field::ScriptPubKey(k) => {
                        self.txs[i]
                            .outputs
                            .get_mut(k)
                            .ok_or(format!("tx {i} has no output {k}"))?
                            .1 = bytes(0)?;
                    }
                    Field::Outputs => self.txs[i].outputs = outputs_of(&bytes(0)?)?,
                    other => return Err(format!("{other:?} on a transaction")),
                }
            }
            (Target::Block, Field::Inserted(o, at, kind)) => {
                if at > self.txs.len() {
                    return Err(format!("an insert at {at} of {}", self.txs.len()));
                }
                let (tx, link) = kind.tx(self.carrier.clone());
                self.links.insert(at, vec![link]);
                self.txs.insert(at, tx);
                self.origin.insert(at, o);
            }
            (Target::Block, Field::Deleted(o)) => {
                let i = self.index(o)?;
                self.txs.remove(i);
                self.origin.remove(i);
                self.links.remove(i);
            }
            (Target::Block, Field::Duplicated(src, dup, at)) => {
                let i = self.index(src)?;
                if at > self.txs.len() {
                    return Err(format!("a duplicate at {at} of {}", self.txs.len()));
                }
                let (tx, links) = (self.txs[i].clone(), self.links[i].clone());
                self.txs.insert(at, tx);
                self.links.insert(at, links);
                self.origin.insert(at, dup);
                self.dup = Some(dup);
            }
            (Target::Block, Field::Swapped(a, b)) => {
                let (i, j) = (self.index(a)?, self.index(b)?);
                self.txs.swap(i, j);
                self.origin.swap(i, j);
                self.links.swap(i, j);
            }
            (Target::Block, Field::Moved(o, to)) => {
                let i = self.index(o)?;
                let (tx, origin, links) =
                    (self.txs.remove(i), self.origin.remove(i), self.links.remove(i));
                if to > self.txs.len() {
                    return Err(format!("a move to {to} of {}", self.txs.len()));
                }
                self.txs.insert(to, tx);
                self.origin.insert(to, origin);
                self.links.insert(to, links);
            }
            (target, field) => return Err(format!("{field:?} on {target:?}")),
        }
        Ok(())
    }
}

/// An output list from [`outputs_bytes`]'.
fn outputs_of(b: &[u8]) -> Result<Vec<(i64, Vec<u8>)>, String> {
    let mut out = Vec::new();
    let mut rest = b;
    while !rest.is_empty() {
        if rest.len() < 12 {
            return Err("a truncated output list".into());
        }
        let value = i64::from_le_bytes(rest[..8].try_into().expect("8 bytes"));
        let len = u32::from_le_bytes(rest[8..12].try_into().expect("4 bytes")) as usize;
        let script = rest.get(12..12 + len).ok_or("a truncated output script")?.to_vec();
        out.push((value, script));
        rest = &rest[12 + len..];
    }
    Ok(out)
}

/// §3.4's changed-field check, on the decoded final bytes (OD-L3-52, OD-L3-53):
/// - the final transactions are the base's decoded ones with the draw's records applied
///   (`Replay`), position by position, which measures every transaction's origin; the
///   draft's declared origins must be those;
/// - the header holds the base's fields but for the recorded ones, and its merkle root is the
///   recorded one, else the root of the final transactions (without a duplicate's copy);
/// - each recorded mutation's field at its target holds the recorded value (a link resolved
///   against the final block) and differs from the base's, and a structural one's relation holds
///   on the measured origins.
///
/// A mutation absent from the final bytes is a harness error, never a pass. Returns the final
/// block decoded (its merkle root as `merkle_override`, re-serializing to the same bytes but the
/// nonce) and the checked mutations.
pub fn check(
    draw: &Draw,
    base: &Draft,
    base_built: &Built,
    built: &Built,
    trial: &[u8; 8],
) -> Result<(BlockPlan, Vec<Checked>), String> {
    let old = Read::new(base_built, &base.origin)?;
    let base_links = measured_links(&old.plan.txs, &base.origin);
    if base_links != base.links {
        return Err("the base's decoded links are not its draft's".into());
    }
    let mut fin_plan = BlockPlan::from_block(&built.bytes)?;
    let header: [u8; 80] = built.bytes[..80].try_into().map_err(|_| "a short block")?;
    let mut replay = Replay {
        origin: base.origin.clone(),
        txs: old.plan.txs.clone(),
        links: base_links,
        dup: None,
        carrier: carrier(trial),
    };
    for m in &draw.mutations {
        replay.apply(m).map_err(|e| format!("{} {:?}: {e}", m.family, m.field))?;
    }
    relink(&mut replay.txs, &replay.origin, &replay.links)?;
    if replay.txs.len() != fin_plan.txs.len() {
        return Err(format!(
            "{} transactions decoded, {} after the records",
            fin_plan.txs.len(),
            replay.txs.len()
        ));
    }
    if let Some(i) = (0..replay.txs.len()).find(|&i| replay.txs[i] != fin_plan.txs[i]) {
        return Err(format!(
            "tx {i} (origin {}) is not the base's with the draw's records (origins {:?})",
            replay.origin[i], replay.origin
        ));
    }
    if replay.origin != draw.draft.origin {
        return Err(format!(
            "the draft declares origins {:?}, the final bytes hold {:?}",
            draw.draft.origin, replay.origin
        ));
    }
    let recorded = |field: Field| {
        draw.mutations.iter().find_map(|m| match (&m.target, m.field == field, &m.new) {
            (Target::Header, true, Val::Bytes(b)) => Some(b.clone()),
            _ => None,
        })
    };
    for (field, range) in
        [(Field::Version, 0..4), (Field::Prev, 4..36), (Field::Time, 68..72), (Field::Bits, 72..76)]
    {
        let want = recorded(field).unwrap_or_else(|| old.header[range.clone()].to_vec());
        if header[range.clone()] != want[..] {
            return Err(format!(
                "header {field:?}: {} not {}",
                to_hex(&header[range]),
                to_hex(&want)
            ));
        }
    }
    let root = match (recorded(Field::Merkle), replay.dup) {
        (Some(root), _) => root,
        (None, dup) => {
            let mut honest = fin_plan.clone();
            if let Some(i) = dup.and_then(|d| replay.origin.iter().position(|o| *o == d)) {
                honest.txs.remove(i);
            }
            merkle_root(&honest).to_vec()
        }
    };
    if header[36..68] != root[..] {
        return Err(format!("header Merkle: {} not {}", to_hex(&header[36..68]), to_hex(&root)));
    }
    let fin = Read {
        header,
        plan: fin_plan.clone(),
        origin: &replay.origin,
    };
    let mut out = Vec::new();
    for m in &draw.mutations {
        let what = format!("{} {:?} {:?}", m.family, m.target, m.field);
        let (was, now) = if m.new == Val::Structure {
            fin.structure(m.field, &old).map_err(|e| format!("{what}: {e}"))?;
            (format!("{:?}", old.origin), format!("{:?}", fin.origin))
        } else {
            let want = match &m.new {
                Val::Bytes(b) => b.clone(),
                Val::Link(o, vout) => {
                    let i =
                        fin.index(*o).ok_or(format!("{what}: its link's origin {o} is gone"))?;
                    outpoint_bytes((fin.plan.txs[i].txid(), *vout))
                }
                Val::Structure => unreachable!("handled above"),
            };
            let got = fin
                .field(m.target, m.field)
                .ok_or(format!("{what}: no such field in the final bytes"))?;
            let was = old.field(m.target, m.field);
            if got != want {
                return Err(format!(
                    "{what}: the final bytes hold {}, the mutation set {}",
                    to_hex(&got),
                    to_hex(&want)
                ));
            }
            if was.as_ref() == Some(&got) {
                return Err(format!("{what}: the final bytes equal the base's ({})", to_hex(&got)));
            }
            (was.map_or_else(|| "absent".into(), |w| to_hex(&w)), to_hex(&got))
        };
        out.push(Checked {
            family: m.family,
            target: m.target,
            field: m.field,
            old: was,
            new: now,
        });
    }
    // The facts' plan (OD-L3-53): the decoded block, which re-serializes to the final bytes but
    // the nonce.
    fin_plan.merkle_override = Some(header[36..68].try_into().expect("32 bytes"));
    let again = fin_plan.finish(false)?;
    if again.bytes[..76] != built.bytes[..76] || again.bytes[80..] != built.bytes[80..] {
        return Err("the decoded block does not re-serialize to the final bytes".into());
    }
    Ok((fin_plan, out))
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;
    use crate::chain::mutate::Site;

    /// Case `n`'s coins as the split lays them down: one split txid, the slots' locks.
    fn coins(seed: u64, n: usize) -> Vec<Coin> {
        slots(seed, n)
            .iter()
            .enumerate()
            .map(|(k, s)| Coin {
                outpoint: ([n as u8; 32], k as u32),
                value: 55_000_000,
                lock: s.lock(),
                height: 101,
                coinbase: false,
            })
            .collect()
    }

    /// The pool: three slots per case from `(seed, n)` alone, slot 0 always spendable, every
    /// kind drawn over a run; a relative lock never deeper than the shallowest slot.
    #[test]
    fn the_pool_is_three_slots_per_case_from_the_seed_alone() {
        let mut kinds = BTreeSet::new();
        for n in 0..2_000 {
            let s = slots(7, n);
            assert_eq!(s, slots(7, n), "case {n}'s slots are its seed's");
            assert!(s[0].spendable(), "case {n}: slot 0 is a puzzle");
            for slot in s {
                if let Slot::Csv(r) | Slot::Bip68(r) = slot {
                    assert!((1..=RELATIVE_MAX).contains(&r));
                }
                kinds.insert(match slot {
                    Slot::Csv(_) => "csv".to_owned(),
                    Slot::Bip68(_) => "bip68".to_owned(),
                    Slot::Sigops(_) => "sigops".to_owned(),
                    other => other.name(),
                });
            }
        }
        let want = [
            "true",
            "p2sh_true",
            "cltv",
            "csv",
            "bip68",
            "sigops",
            "puzzle_P2pkhBadSig",
            "puzzle_P2shBadSig",
            "puzzle_NonDerNot",
            "puzzle_False",
        ];
        for k in want {
            assert!(kinds.contains(k), "no {k} slot in 2,000 cases: {kinds:?}");
        }
        let pool = |seed| (0..2_000).map(|n| slots(seed, n)).collect::<Vec<_>>();
        assert_ne!(pool(7), pool(8), "another seed, another pool");
        let last = pool_heights(MAX_CASES).end - 1;
        assert!(last + u32::from(RELATIVE_MAX) <= FIRST_HEIGHT, "a relative lock unmet at {last}");
    }

    /// A population: one to six transactions, each spending a spendable slot (with the slot's
    /// sequence and lock time) or an earlier output it links to; no slot or output spent twice,
    /// no puzzle spent; one to three value outputs then the carrier, paying what it spends less
    /// the fee. Spend chains: about half of the transactions after the first spend their
    /// predecessor.
    #[test]
    fn populations_spend_their_slots_and_chain() {
        let trial = [9u8; 8];
        let (mut sizes, mut chained, mut later) = (BTreeSet::new(), 0, 0);
        for n in 0..2_000 {
            let coins = coins(3, n);
            let kinds = slots(3, n);
            let pop = population(3, n, &coins, &trial).expect("a population");
            assert_eq!(Ok(&pop), population(3, n, &coins, &trial).as_ref(), "deterministic");
            assert!((1..=6).contains(&pop.len()), "case {n}: {} transactions", pop.len());
            sizes.insert(pop.len());
            let mut spent = BTreeSet::new();
            for (i, t) in pop.iter().enumerate() {
                assert_eq!(t.origin, i as u32 + 1);
                assert_eq!(t.tx.outputs.last(), Some(&carrier(&trial)));
                let outs = &t.tx.outputs[..t.tx.outputs.len() - 1];
                assert!((1..=3).contains(&outs.len()));
                let rest = (t.value_in - FEE) % outs.len() as i64;
                assert_eq!(outs.iter().map(|o| o.0).sum::<i64>() + FEE + rest, t.value_in);
                let mut value = 0;
                for (input, link) in t.tx.inputs.iter().zip(&t.links) {
                    assert!(
                        spent.insert(input.prevout),
                        "case {n}: {:?} spent twice",
                        input.prevout
                    );
                    match link {
                        Some((o, vout)) => {
                            let parent = &pop[*o as usize - 1];
                            assert!(*o < t.origin, "a link points back");
                            assert_eq!(input.prevout, (parent.tx.txid(), *vout));
                            assert_eq!(input.sequence, u32::MAX);
                            value += parent.tx.outputs[*vout as usize].0;
                        }
                        None => {
                            let k = coins
                                .iter()
                                .position(|c| c.outpoint == input.prevout)
                                .expect("a slot");
                            assert!(kinds[k].spendable(), "case {n}: a puzzle spent");
                            assert_eq!(input.sequence, kinds[k].sequence());
                            assert_eq!(input.script_sig, kinds[k].lock().script_sig());
                            if kinds[k] == Slot::Cltv {
                                assert_eq!(t.tx.lock_time, CLTV_LOCK);
                            }
                            value += coins[k].value;
                        }
                    }
                }
                assert_eq!(value, t.value_in);
                if i > 0 {
                    later += 1;
                    chained +=
                        usize::from(t.links.iter().flatten().any(|(o, _)| *o == t.origin - 1));
                }
            }
        }
        assert_eq!(sizes, (1..=6).collect(), "every population size");
        let share = chained as f64 / later as f64;
        assert!((0.4..0.8).contains(&share), "spend chains: {chained} of {later}");
    }

    /// A population refuses coins that are not its slots.
    #[test]
    fn a_population_needs_its_own_slots() {
        let mut wrong = coins(3, 5);
        wrong[0].lock = Lock::NullDummy;
        assert!(population(3, 5, &wrong, &[0; 8]).is_err());
        assert!(population(3, 5, &wrong[..2], &[0; 8]).is_err());
    }

    /// The run's budget: N cases at `PER_HEIGHT` per height from `FIRST_HEIGHT`, all on Reg and
    /// below DIP3 (OD-L3-65 n2); one more than `MAX_CASES` is refused, as are 0 cases, 0 shards
    /// and more shards than `PER_HEIGHT` (n5).
    #[test]
    fn the_cases_fit_reg_or_the_run_refuses() {
        let max = Config {
            n: MAX_CASES,
            ..DEFAULT
        };
        assert_eq!(max.check(), Ok(()));
        assert!((0..MAX_CASES).all(|n| Site::Reg.holds(max.height(n))));
        assert!(!Site::Reg.holds(max.height(MAX_CASES)));
        for bad in [
            Config {
                n: MAX_CASES + 1,
                ..DEFAULT
            },
            Config {
                n: 0,
                ..DEFAULT
            },
            Config {
                shards: 0,
                ..DEFAULT
            },
            Config {
                shards: PER_HEIGHT + 1,
                ..DEFAULT
            },
            Config {
                shards: 500,
                ..DEFAULT
            },
        ] {
            assert!(bad.check().is_err(), "{bad:?}");
        }
        let widest = Config {
            shards: PER_HEIGHT,
            ..DEFAULT
        };
        assert_eq!(widest.check(), Ok(()));
        assert!(max.height(MAX_CASES - 1) < DIP3_HEIGHT, "a case at or past DIP3");
        assert_eq!(DEFAULT.check(), Ok(()));
        assert_eq!((0..5).map(|n| DEFAULT.shard(n)).collect::<Vec<_>>(), [0, 1, 2, 3, 0]);
    }

    /// The stream: deterministic per parts, different across parts.
    #[test]
    fn streams_are_their_parts() {
        let take = |parts: &[u64]| {
            let mut r = Rng::new(parts);
            (0..4).map(|_| r.word()).collect::<Vec<_>>()
        };
        assert_eq!(take(&[1, 2, 3]), take(&[1, 2, 3]));
        assert_ne!(take(&[1, 2, 3]), take(&[1, 2, 4]));
        assert_ne!(take(&[1, 2, 3]), take(&[2, 1, 3]));
    }

    /// A synthetic base for case `n`: a tagged coinbase on a random parent at height 300, then
    /// the population on the case's slots.
    fn base(seed: u64, n: usize, trial: [u8; 8]) -> (Draft, DrawCx) {
        let mut rng = Rng::new(&[seed, 0xba5e, n as u64]);
        let coins = coins(seed, n);
        let pop = population(seed, n, &coins, &trial).expect("a population");
        let mut cb = crate::chain::build::coinbase(300, 50_000_000_000, 7);
        cb.inputs[0].script_sig.extend(push(&trial));
        let template = BlockPlan {
            prev: rng.bytes32(),
            version: 0x2000_0000,
            time: 1_700_045_000,
            bits: REGTEST_BITS,
            txs: vec![cb],
            merkle_override: None,
        };
        let puzzles = slots(seed, n)
            .iter()
            .zip(&coins)
            .filter(|(s, _)| !s.spendable())
            .map(|(_, c)| c.outpoint)
            .collect();
        let cx = DrawCx {
            height: 300,
            mtp: 1_700_044_000,
            now: 1_700_045_000,
            grandparent: rng.bytes32(),
            genesis: rng.bytes32(),
            spent: (rng.bytes32(), 0),
            immature: (rng.bytes32(), 0),
            puzzles,
            slots: coins.iter().map(|c| c.outpoint).collect(),
            trial,
        };
        (Draft::base(&template, &pop), cx)
    }

    /// A draw as [`pick`] measures it (OD-L3-53): finalized, then (unless it is its base) checked
    /// on its decoded final bytes, then the regime filter on the decoded block. `None` for a
    /// draw equal to its base; a finalization or check that fails panics.
    fn measured(
        seed: u64,
        n: usize,
        r: u32,
        d: &Draft,
        cx: &DrawCx,
    ) -> Option<(Draw, BlockPlan, Option<String>)> {
        let drawn = draw(seed, n, r, d, cx);
        let (_, base_built) = d.finish().expect("the base");
        let (_, built) = drawn
            .draft
            .finish()
            .unwrap_or_else(|e| panic!("draw {n}/{r}: {e} ({:?})", drawn.mutations));
        if drawn.mutations.is_empty() || built.bytes == base_built.bytes {
            return None;
        }
        let (plan, checked) = check(&drawn, d, &base_built, &built, &cx.trial)
            .unwrap_or_else(|e| panic!("draw {n}/{r}: {e} ({:?})", drawn.mutations));
        assert_eq!(checked.len(), drawn.mutations.len());
        let refused = regime_refusal(&plan, cx.height);
        Some((drawn, plan, refused))
    }

    /// The regime filter (§3.4, DEC-1059) over 10^5 draws, each measured as [`pick`] does: no
    /// draw it passes holds, in its decoded final bytes, a special version ≥ 3 with nType ≠ 0, a
    /// type-5 coinbase or (below DIP3) a non-coinbase nType 6 below version 3, each read
    /// independently with the coinbase predicate; every draw it refuses holds one; and it does
    /// refuse draws, so it is live. Every draw's records pass the check, refused ones included.
    #[test]
    fn the_regime_filter_holds_over_100k_draws() {
        // The base's height (300) is below DIP3's, so DEC-1059's selector applies too.
        let qc = |t: &TxPlan| !is_coinbase(t) && t.tx_type == 6 && t.version < 3;
        let escapes = |plan: &BlockPlan| {
            plan.txs.iter().any(|t| {
                (t.version >= 3 && t.tx_type != 0) || (is_coinbase(t) && t.tx_type == 5) || qc(t)
            })
        };
        let counts: Vec<(usize, usize, usize)> = std::thread::scope(|s| {
            let chunks: Vec<_> = (0..16usize)
                .map(|c| {
                    s.spawn(move || {
                        let (mut refused, mut refused_qc, mut type6_coinbase) = (0, 0, 0);
                        for k in (c..100_000usize).step_by(16) {
                            let (n, r) = (k / 4, (k % 4) as u32);
                            let (d, cx) = base(11, n % 500, [k as u8; 8]);
                            let Some((drawn, plan, why)) = measured(11, n, r, &d, &cx) else {
                                continue;
                            };
                            assert_eq!(why.is_some(), escapes(&plan), "draw {n}/{r}: {why:?}");
                            // OD-L3-65 n1: a type-6 coinbase never connects; at vtx[0] it took
                            // all three of the draw's mutations.
                            if let Some(at) = plan.txs.iter().position(|t| {
                                is_coinbase(t) && t.tx_type == TRANSACTION_QUORUM_COMMITMENT
                            }) {
                                assert!(
                                    before_connect(&plan, cx.height).is_some(),
                                    "draw {n}/{r}: a type-6 coinbase at {at} connects"
                                );
                                if at == 0 {
                                    let families: BTreeSet<&str> =
                                        drawn.mutations.iter().map(|m| m.family).collect();
                                    assert_eq!(
                                        families,
                                        BTreeSet::from([
                                            "tx.version",
                                            "tx.prevout",
                                            "blk.structure"
                                        ]),
                                        "draw {n}/{r}"
                                    );
                                }
                                type6_coinbase += 1;
                            }
                            if let Some(why) = why {
                                refused += 1;
                                refused_qc += usize::from(why.starts_with(QC_TYPE_PRE_DIP3));
                            }
                        }
                        (refused, refused_qc, type6_coinbase)
                    })
                })
                .collect();
            chunks.into_iter().map(|h| h.join().expect("a chunk")).collect()
        });
        let refused: usize = counts.iter().map(|c| c.0).sum();
        let refused_qc: usize = counts.iter().map(|c| c.1).sum();
        assert!(refused > 100, "the filter refused {refused} of 10^5 draws: not live");
        assert!(refused_qc > 0, "DEC-1059's selector refused none of 10^5 draws: not live");
        let type6_coinbase: usize = counts.iter().map(|c| c.2).sum();
        eprintln!(
            "regime filter: {refused} of 10^5 draws refused, {refused_qc} by {QC_TYPE_PRE_DIP3}; \
             {type6_coinbase} with a type-6 coinbase, none connecting"
        );
    }

    /// DEC-1059's selector is exactly a pre-DIP3, non-coinbase `nType == 6` with `nVersion < 3`:
    /// not at or past DIP3, not on a coinbase, not another type, not a special version.
    #[test]
    fn the_qc_type_selector_is_narrow() {
        let (d, _) = base(5, 0, [0; 8]);
        let with = |i: usize, version: i16, tx_type: u16| {
            let mut plan = d.plan.clone();
            (plan.txs[i].version, plan.txs[i].tx_type) = (version, tx_type);
            plan
        };
        let reason = |plan: &BlockPlan, h: u32| regime_refusal(plan, h);
        assert!(reason(&with(1, 0, 6), 300).is_some_and(|r| r.starts_with(QC_TYPE_PRE_DIP3)));
        assert!(
            reason(&with(1, 2, 6), DIP3_HEIGHT - 1)
                .is_some_and(|r| r.starts_with(QC_TYPE_PRE_DIP3))
        );
        assert_eq!(reason(&with(1, 0, 6), DIP3_HEIGHT), None, "at DIP3: not refused");
        assert_eq!(reason(&with(0, 0, 6), 300), None, "the coinbase: not this selector");
        assert_eq!(reason(&with(1, 0, 7), 300), None, "another type: not refused");
        assert_eq!(reason(&with(1, 0, 0), 300), None);
        // A special version with type 6 is the §3.4 filter's own refusal.
        assert!(reason(&with(1, 3, 6), 300).is_some_and(|r| !r.starts_with(QC_TYPE_PRE_DIP3)));
    }

    /// OD-L3-54: a coinbase is one by its predicate, never by its index: a non-coinbase at slot
    /// 0, a coinbase later in the list, and an inserted coinbase are each judged for what they
    /// are, by the filter and by the families' target choice.
    #[test]
    fn a_coinbase_is_its_predicate_not_its_index() {
        let (d, _) = base(5, 0, [0; 8]);
        assert!(d.plan.txs.len() >= 2 && is_coinbase(&d.plan.txs[0]));
        // A non-coinbase v0 / type 6 moved to slot 0, the coinbase to slot 1: refused.
        let mut front = d.plan.clone();
        (front.txs[1].version, front.txs[1].tx_type) = (0, 6);
        front.txs.swap(0, 1);
        assert_eq!(
            regime_refusal(&front, 300).as_deref(),
            Some(format!("{QC_TYPE_PRE_DIP3}: tx 0").as_str())
        );
        // A coinbase later in the list: a type 6 one is not this selector's, a type 5 one is
        // the type-5 rule's wherever it stands.
        let mut later = d.plan.clone();
        later.txs.swap(0, 1);
        (later.txs[1].version, later.txs[1].tx_type) = (0, 6);
        assert_eq!(regime_refusal(&later, 300), None, "a coinbase at 1: not refused");
        later.txs[1].tx_type = TRANSACTION_COINBASE;
        assert_eq!(regime_refusal(&later, 300).as_deref(), Some("tx 1: a type-5 coinbase"));
        // A non-coinbase of type 5 at slot 0 is not a type-5 coinbase.
        let mut five = d.plan.clone();
        five.txs[1].tx_type = TRANSACTION_COINBASE;
        five.txs.swap(0, 1);
        assert_eq!(regime_refusal(&five, 300), None);
        // An inserted coinbase (null prevout) after the original: of type 6 it is not refused,
        // and no `tx.*` family targets it; a spend at slot 0 is still a target.
        let mut inserted = d.clone();
        let mut cb = TxPlan::simple(
            vec![Input::new(([0; 32], u32::MAX), vec![1, 2])],
            vec![(1_000, Lock::P2shTrue.script_pubkey())],
        );
        (cb.version, cb.tx_type) = (0, 6);
        inserted.plan.txs.insert(1, cb);
        inserted.origin.insert(1, INSERTED);
        inserted.links.insert(1, vec![None]);
        inserted.value_in.insert(1, 0);
        assert_eq!(regime_refusal(&inserted.plan, 300), None);
        assert!(!ordinary(&inserted).contains(&INSERTED));
        assert!(!ordinary(&inserted).contains(&COINBASE_ORIGIN));
        let mut moved = d.clone();
        moved.plan.txs.swap(0, 1);
        moved.origin.swap(0, 1);
        assert!(ordinary(&moved).contains(&d.origin[1]));
        assert!(!ordinary(&moved).contains(&COINBASE_ORIGIN));
    }

    /// Every family applies and its mutations pass the changed-field check on the final bytes;
    /// each mutation's field holds its value there and differs from the base's.
    #[test]
    fn every_family_applies_and_checks_on_the_final_bytes() {
        let mut verified: BTreeMap<&str, usize> = BTreeMap::new();
        for n in 0..3_000 {
            let (d, cx) = base(5, n, [n as u8, 1, 2, 3, 4, 5, 6, 7]);
            // `measured` checks every draw's records, refused ones included.
            let Some((drawn, _, None)) = measured(5, n, 0, &d, &cx) else {
                continue;
            };
            for m in drawn.mutations {
                *verified.entry(m.family).or_default() += 1;
            }
        }
        for family in FAMILIES {
            assert!(verified.get(family).is_some_and(|v| *v >= 40), "{family}: {verified:?}");
        }
    }

    /// The check's power (the generator mutants' shapes, G1–G3): a dropped mutation, one that
    /// landed on another transaction, a re-root over an intentional merkle root, and (OD-L3-65
    /// n7) one that landed on another field of the right transaction each fail it.
    #[test]
    fn the_check_fails_a_dropped_moved_or_rerooted_mutation() {
        const T1: [u8; 8] = [1; 8];
        let (d, _) = base(5, 1, T1);
        let (_, base_built) = d.finish().expect("the base");
        let with = |m: Mutation, draft: Draft| Draw {
            r: 0,
            drawn: vec![m.family],
            mutations: vec![m],
            draft,
        };
        let origin = d.origin[1];
        let mut sequence = d.clone();
        sequence.plan.txs[1].inputs[0].sequence = 7;
        let m = Mutation {
            family: "tx.sequence",
            target: Target::Tx(origin),
            field: Field::Sequence(0),
            new: Val::Bytes(7u32.to_le_bytes().to_vec()),
        };
        let ok = with(m.clone(), sequence.clone());
        let (_, built) = ok.draft.finish().expect("final");
        assert!(check(&ok, &d, &base_built, &built, &T1).is_ok());
        // G1: the mutation is recorded, the base returned.
        let dropped = with(m.clone(), d.clone());
        let (_, built) = dropped.draft.finish().expect("final");
        assert!(
            check(&dropped, &d, &base_built, &built, &T1).is_err(),
            "a dropped mutation passed"
        );
        // G2: it landed on the coinbase instead.
        let mut elsewhere = d.clone();
        elsewhere.plan.txs[0].inputs[0].sequence = 7;
        let moved = with(m, elsewhere);
        let (_, built) = moved.draft.finish().expect("final");
        assert!(
            check(&moved, &d, &base_built, &built, &T1).is_err(),
            "a misplaced mutation passed"
        );
        // G3: finalization re-roots over a hdr.merkle value.
        let mut merkle = d.clone();
        merkle.merkle = Merkle::Fixed([0x5a; 32]);
        let m = Mutation {
            family: "hdr.merkle",
            target: Target::Header,
            field: Field::Merkle,
            new: Val::Bytes(vec![0x5a; 32]),
        };
        let fixed = with(m.clone(), merkle.clone());
        let (_, built) = fixed.draft.finish().expect("final");
        assert!(check(&fixed, &d, &base_built, &built, &T1).is_ok());
        let mut rerooted = merkle;
        rerooted.merkle = Merkle::Honest;
        let rerooted = with(m, rerooted);
        let (_, built) = rerooted.draft.finish().expect("final");
        assert!(check(&rerooted, &d, &base_built, &built, &T1).is_err(), "a re-root passed");
        // n7: the right transaction, another field. A base whose tx 1 has two inputs and two
        // value outputs, each value written where the record says another field holds it.
        let (d, base_built) = (0..300)
            .map(|n| base(5, n, T1).0)
            .find(|d| {
                d.plan.txs.len() >= 2
                    && d.plan.txs[1].inputs.len() >= 2
                    && d.plan.txs[1].outputs.len() >= 3
            })
            .map(|d| {
                let built = d.finish().expect("the base").1;
                (d, built)
            })
            .expect("a population tx with two inputs and two value outputs");
        let target = Target::Tx(d.origin[1]);
        let wrong =
            |family: &'static str, field: Field, new: Vec<u8>, set: &dyn Fn(&mut TxPlan)| {
                let mut x = d.clone();
                set(&mut x.plan.txs[1]);
                let draw = with(
                    Mutation {
                        family,
                        target,
                        field,
                        new: Val::Bytes(new),
                    },
                    x,
                );
                let (_, built) = draw.draft.finish().expect("final");
                check(&draw, &d, &base_built, &built, &T1)
            };
        let seq = 7u32.to_le_bytes().to_vec();
        assert!(
            wrong("tx.sequence", Field::Sequence(0), seq.clone(), &|t| t.inputs[0].sequence = 7)
                .is_ok()
        );
        assert!(
            wrong("tx.sequence", Field::Sequence(0), seq, &|t| t.inputs[1].sequence = 7).is_err(),
            "a sequence on input 1 recorded on input 0 passed"
        );
        let value = le(1_234);
        assert!(
            wrong("tx.value", Field::Value(0), value.clone(), &|t| t.outputs[0].0 = 1_234).is_ok()
        );
        assert!(
            wrong("tx.value", Field::Value(0), value, &|t| t.outputs[1].0 = 1_234).is_err(),
            "a value on output 1 recorded on output 0 passed"
        );
        let script = vec![OP_RETURN];
        assert!(
            wrong("tx.script", Field::ScriptPubKey(0), script.clone(), &|t| {
                t.outputs[0].1 = vec![OP_RETURN];
            })
            .is_ok()
        );
        assert!(
            wrong("tx.script", Field::ScriptSig(0), script, &|t| t.outputs[0].1 = vec![OP_RETURN])
                .is_err(),
            "a scriptPubKey write recorded as a scriptSig passed"
        );
    }

    /// §3.4's no-op handling: a draw whose block a side knows is re-drawn from the next
    /// sub-seed; four such draws make the case a typed no-op, never a pass. A draw equal to its
    /// base is a no-op too.
    #[test]
    fn known_or_unchanged_draws_are_redrawn_then_typed_noops() {
        let (d, cx) = base(5, 2, [2; 8]);
        let (_, built) = d.finish().expect("the base");
        let bases = [Base {
            draft: d.clone(),
            built: built.clone(),
            cx: cx.clone(),
        }];
        let mut calls = 0;
        let first_known = pick(5, 2, &bases, |_| {
            calls += 1;
            Ok(calls == 1)
        })
        .expect("an outcome");
        match first_known {
            Outcome::Picked {
                r,
                tried,
                ..
            } => {
                assert!(r >= 1);
                assert!(tried.iter().any(|t| t.reason.starts_with("noop: a known hash")));
            }
            other => panic!("{other:?}"),
        }
        match pick(5, 2, &bases, |_| Ok(true)).expect("an outcome") {
            Outcome::Noop {
                tried,
            } => assert_eq!(tried.len(), DRAWS as usize),
            other => panic!("{other:?}"),
        }
        assert!(pick(5, 2, &bases, |_| Err("down".into())).is_err(), "a harness error passed");
        // A draw with no mutation (every family skipped) is the base: a no-op.
        let empty = Draw {
            r: 0,
            drawn: vec!["blk.structure"],
            mutations: Vec::new(),
            draft: d.clone(),
        };
        assert_eq!(empty.draft.finish().expect("final").1.bytes, built.bytes);
    }

    /// A base with at least three transactions (the coinbase and two spends) and its pick base.
    fn wide_base() -> (Draft, DrawCx, Built) {
        let (d, cx) = (0..200)
            .map(|n| base(5, n, [n as u8; 8]))
            .find(|(d, _)| d.plan.txs.len() >= 3)
            .expect("a population of two or more");
        let (_, built) = d.finish().expect("the base");
        (d, cx, built)
    }

    /// A draw of `mutations` on `draft`, with an applied `hdr.time` that keeps it off its base.
    fn crafted(base: &Draft, mut draft: Draft, mut mutations: Vec<Mutation>) -> Draw {
        draft.plan.time = base.plan.time + 1;
        mutations.push(Mutation {
            family: "hdr.time",
            target: Target::Header,
            field: Field::Time,
            new: Val::Bytes((base.plan.time + 1).to_le_bytes().to_vec()),
        });
        Draw {
            r: 0,
            drawn: mutations.iter().map(|m| m.family).collect(),
            mutations,
            draft,
        }
    }

    /// `pick` over one base whose every draw is `d`.
    fn pick_one(d: &Draft, cx: &DrawCx, built: &Built, draw: &Draw) -> Result<Outcome, String> {
        let bases = [Base {
            draft: d.clone(),
            built: built.clone(),
            cx: cx.clone(),
        }];
        pick_draws(&bases, |_| vec![draw.clone()], |_| Ok(false))
    }

    fn structural(field: Field) -> Mutation {
        Mutation {
            family: "blk.structure",
            target: Target::Block,
            field,
            new: Val::Structure,
        }
    }

    /// OD-L3-52: a structural record holds only where the decoded final transactions show it.
    /// Each operation done is picked; the same origin bookkeeping with the transaction operation
    /// omitted (another applied mutation keeping the block off its base) is a harness error
    /// before any credit.
    #[test]
    fn a_structural_record_is_read_from_the_decoded_order() {
        let (d, cx, built) = wide_base();
        let len = d.plan.txs.len();
        let picked =
            |draw: &Draw| matches!(pick_one(&d, &cx, &built, draw), Ok(Outcome::Picked { .. }));
        let failed = |draw: &Draw| pick_one(&d, &cx, &built, draw).is_err();
        // Swap 1 and 2.
        let swap = |txs: bool| {
            let mut x = d.clone();
            if txs {
                x.plan.txs.swap(1, 2);
            }
            x.origin.swap(1, 2);
            x.links.swap(1, 2);
            x.value_in.swap(1, 2);
            let m = structural(Field::Swapped(x.origin[2], x.origin[1]));
            crafted(&d, x, vec![m])
        };
        assert!(picked(&swap(true)), "a swap done");
        assert!(failed(&swap(false)), "a swap recorded, not done, was credited");
        // Move 1 to the end.
        let mv = |txs: bool| {
            let mut x = d.clone();
            if txs {
                let tx = x.plan.txs.remove(1);
                x.plan.txs.push(tx);
            }
            let (o, l, v) = (x.origin.remove(1), x.links.remove(1), x.value_in.remove(1));
            x.origin.push(o);
            x.links.push(l);
            x.value_in.push(v);
            crafted(&d, x, vec![structural(Field::Moved(o, len - 1))])
        };
        assert!(picked(&mv(true)), "a move done");
        assert!(failed(&mv(false)), "a move recorded, not done, was credited");
        // Insert a spend of an unknown outpoint at 1.
        let unknown = Insert::Unknown([0x77; 32]);
        let ins = |txs: bool| {
            let mut x = d.clone();
            if txs {
                x.plan.txs.insert(1, unknown.tx(carrier(&cx.trial)).0);
            }
            x.origin.insert(1, INSERTED);
            x.links.insert(1, vec![None]);
            x.value_in.insert(1, 0);
            crafted(&d, x, vec![structural(Field::Inserted(INSERTED, 1, unknown))])
        };
        assert!(picked(&ins(true)), "an insert done");
        assert!(failed(&ins(false)), "an insert recorded, not done, was credited");
        // Delete the last spend.
        let del = |txs: bool| {
            let mut x = d.clone();
            if txs {
                x.plan.txs.remove(len - 1);
            }
            let o = x.origin.remove(len - 1);
            x.links.remove(len - 1);
            x.value_in.remove(len - 1);
            crafted(&d, x, vec![structural(Field::Deleted(o))])
        };
        assert!(picked(&del(true)), "a delete done");
        assert!(failed(&del(false)), "a delete recorded, not done, was credited");
        // Duplicate 1 at the end (the header keeps the root without the copy).
        let dup = |txs: bool| {
            let mut x = d.clone();
            if txs {
                let tx = x.plan.txs[1].clone();
                x.plan.txs.push(tx);
            }
            let source = x.origin[1];
            x.origin.push(INSERTED);
            x.links.push(x.links[1].clone());
            x.value_in.push(x.value_in[1]);
            x.merkle = Merkle::BeforeDuplicate(INSERTED);
            crafted(&d, x, vec![structural(Field::Duplicated(source, INSERTED, len))])
        };
        assert!(picked(&dup(true)), "a duplicate done");
        assert!(failed(&dup(false)), "a duplicate recorded, not done, was credited");
        // An unrecorded swap of the transactions (the origins kept) fails too.
        let mut silent = d.clone();
        silent.plan.txs.swap(1, 2);
        assert!(failed(&crafted(&d, silent, Vec::new())), "an unrecorded swap was credited");
    }

    /// OD-L3-65 n4: an inserted transaction is rebuilt from its record (kind, link) and the
    /// trial's carrier, never read from the final bytes: each kind as drawn is picked; a spend of
    /// another parent than the recorded one, a stray unrecorded write to it, or another kind than
    /// the recorded one is a harness error.
    #[test]
    fn an_inserted_transaction_is_its_record() {
        let (d, cx, built) = wide_base();
        let (p, q) = (d.origin[1], d.origin[2]);
        let insert = |kind: Insert, edit: &dyn Fn(&mut Draft)| {
            let mut x = d.clone();
            let (mut tx, link) = kind.tx(carrier(&cx.trial));
            if let Insert::Spend(parent) = kind {
                tx.inputs[0].prevout = (d.plan.txs[d.index(parent).expect("a parent")].txid(), 0);
            }
            x.plan.txs.insert(1, tx);
            x.origin.insert(1, INSERTED);
            x.links.insert(1, vec![link]);
            x.value_in.insert(1, 0);
            edit(&mut x);
            crafted(&d, x, vec![structural(Field::Inserted(INSERTED, 1, kind))])
        };
        let outcome = |draw: &Draw| pick_one(&d, &cx, &built, draw);
        let coinbase = Insert::Coinbase {
            height: cx.height,
            word: 0x1234,
        };
        for kind in [coinbase, Insert::Unknown([0x77; 32]), Insert::Spend(p)] {
            assert!(
                matches!(outcome(&insert(kind, &|_| {})), Ok(Outcome::Picked { .. })),
                "{kind:?}: {:?}",
                outcome(&insert(kind, &|_| {}))
            );
        }
        // The recorded parent is p; the transaction spends q.
        let wrong_parent = insert(Insert::Spend(p), &|x| x.links[1][0] = Some((q, 0)));
        let e = outcome(&wrong_parent).expect_err("a spend of another parent was credited");
        assert!(e.contains("is not the base's with the draw's records"), "{e}");
        // A stray write: its sequence, which no record names.
        let stray = insert(Insert::Spend(p), &|x| x.plan.txs[1].inputs[0].sequence = 7);
        assert!(outcome(&stray).is_err(), "an unrecorded write to an insert was credited");
        // Another kind's content under the record: an unknown spend recorded as a coinbase.
        let mut other = insert(Insert::Unknown([0x77; 32]), &|_| {});
        other.mutations[0].field = Field::Inserted(INSERTED, 1, coinbase);
        assert!(outcome(&other).is_err(), "another kind of insert was credited");
    }

    /// OD-L3-65 n3: a case's trials draw the same mutations (family, target, field); a trial
    /// whose draw differs is a harness error, not a pairing of unlike blocks.
    #[test]
    fn a_case_s_trials_draw_the_same_mutations() {
        let (d, cx, built) = wide_base();
        let bases: Vec<Base> = (0..2)
            .map(|_| Base {
                draft: d.clone(),
                built: built.clone(),
                cx: cx.clone(),
            })
            .collect();
        let at = |o: u32, field: Field| {
            let mut x = d.clone();
            let i = d.index(o).expect("present");
            match field {
                Field::LockTime => x.plan.txs[i].lock_time = 77,
                _ => x.plan.txs[i].inputs[0].sequence = 77,
            }
            let m = Mutation {
                family: if field == Field::LockTime {
                    "tx.locktime"
                } else {
                    "tx.sequence"
                },
                target: Target::Tx(o),
                field,
                new: Val::Bytes(77u32.to_le_bytes().to_vec()),
            };
            crafted(&d, x, vec![m])
        };
        let (p, q) = (d.origin[1], d.origin[2]);
        let same = pick_draws(
            &bases,
            |_| vec![at(p, Field::LockTime), at(p, Field::LockTime)],
            |_| Ok(false),
        );
        assert!(matches!(same, Ok(Outcome::Picked { .. })), "{same:?}");
        for other in [at(q, Field::LockTime), at(p, Field::Sequence(0))] {
            let e =
                pick_draws(&bases, |_| vec![at(p, Field::LockTime), other.clone()], |_| Ok(false))
                    .expect_err("unlike draws were paired");
            assert!(e.contains("trial 1's mutations"), "{e}");
        }
    }

    /// OD-L3-65 n1: a type-6 coinbase a draw can make never connects, so DEC-1059's selector may
    /// skip coinbases. Every scriptSig a population tx can hold (its slot's or a chain output's)
    /// and every scriptSig `tx.script` writes fails Core's coinbase checks (`bad-cb-length`, or
    /// BIP34's `bad-cb-height`) at every height a generated block can have (the case heights, one
    /// below them, and 1 for a `hdr.prev` of genesis; not at 81, where P2SH true's scriptSig is
    /// BIP34's, a height no generated block has); the shape itself (type 6, its input nulled, the
    /// template coinbase deleted) passes the filter and fails there. Over 10^5 draws
    /// (`the_regime_filter_holds_over_100k_draws`) a type-6 coinbase anywhere fails before connect,
    /// and at vtx[0] it took all three mutations.
    #[test]
    fn a_generated_type_6_coinbase_never_connects() {
        let mut scripts: Vec<Vec<u8>> = vec![
            Lock::True.script_sig(),
            Lock::P2shTrue.script_sig(),
            Lock::Cltv(CLTV_LOCK).script_sig(),
            Vec::new(),
            vec![OP_0],
            vec![0x61],
        ];
        scripts.extend((1..=u32::from(RELATIVE_MAX)).map(|r| Lock::Csv(r).script_sig()));
        scripts.extend((1..=SIGOPS_MAX).map(|k| Lock::Sigops(k).script_sig()));
        // `tx.script`'s three-byte push, every first byte it can start with.
        scripts.push(push(&[0xff; 3]));
        // The heights a generated block can have: the case's, its grandparent's child (a
        // `hdr.prev` of the tip's parent) and genesis's child.
        let heights = std::iter::once(1).chain(FIRST_HEIGHT - 1..BUDGET_START);
        for script in &scripts {
            for height in heights.clone() {
                assert!(
                    coinbase_fails(script, height).is_some(),
                    "{} passes the coinbase checks at {height}",
                    to_hex(script)
                );
            }
        }
        // Why only those heights: P2SH-true's scriptSig is `CScript() << 81`, so a type-6
        // coinbase spending a P2SH-true output would pass BIP34 at height 81, which no generated
        // block has.
        assert_eq!(Lock::P2shTrue.script_sig(), script_num(81));
        assert_eq!(coinbase_fails(&Lock::P2shTrue.script_sig(), 81), None);
        let (d, _) = base(5, 0, [0; 8]);
        let mut shape = d.plan.clone();
        let t = &mut shape.txs[1];
        (t.version, t.tx_type) = (0, TRANSACTION_QUORUM_COMMITMENT);
        t.inputs.truncate(1);
        t.inputs[0].prevout = ([0; 32], u32::MAX);
        shape.txs.remove(0);
        assert!(is_coinbase(&shape.txs[0]));
        assert_eq!(regime_refusal(&shape, 300), None, "the selector skips a coinbase");
        assert!(before_connect(&shape, 300).is_some());
    }

    /// Core's first refusal of a block's coinbase structure before connect, if any: CheckBlock's
    /// `bad-cb-missing` / `bad-cb-multiple` (rc.5 validation.cpp:4409, :4412), CheckTransaction's
    /// `bad-cb-length` and ContextualCheckBlock's BIP34 `bad-cb-height` (both in
    /// [`coinbase_fails`]).
    fn before_connect(plan: &BlockPlan, height: u32) -> Option<&'static str> {
        if !plan.txs.first().is_some_and(is_coinbase) {
            return Some("bad-cb-missing");
        }
        if plan.txs[1..].iter().any(is_coinbase) {
            return Some("bad-cb-multiple");
        }
        coinbase_fails(&plan.txs[0].inputs[0].script_sig, height)
    }

    /// A non-CbTx coinbase scriptSig's failure: 2..=100 bytes (rc.5 consensus/tx_check.cpp:66-67),
    /// then BIP34's prefix `CScript() << nHeight` (validation.cpp:4592-4596; active from height 1
    /// on regtest, chainparams.cpp:816).
    fn coinbase_fails(script: &[u8], height: u32) -> Option<&'static str> {
        if !(2..=100).contains(&script.len()) {
            return Some("bad-cb-length");
        }
        (!script.starts_with(&script_num(i64::from(height)))).then_some("bad-cb-height")
    }

    /// OD-L3-65 n6: `tx.prevout`'s next-output option names only an outpoint the case owns: an
    /// in-block parent's, or another of its own pool slots, never the next case's slot.
    #[test]
    fn the_next_output_option_stays_in_the_case() {
        let mut offered = 0;
        for n in 0..3_000 {
            let (d, cx) = base(5, n, [n as u8; 8]);
            let drawn = draw(5, n, 0, &d, &cx);
            for m in &drawn.mutations {
                let (Target::Tx(o), Field::Prevout(j), Val::Bytes(b)) = (m.target, m.field, &m.new)
                else {
                    continue;
                };
                let Some(i) = d.index(o) else {
                    continue;
                };
                let Some(input) = d.plan.txs[i].inputs.get(j) else {
                    continue;
                };
                let next = (input.prevout.0, input.prevout.1.wrapping_add(1));
                if outpoint_of(b) == next {
                    offered += 1;
                    assert!(
                        d.links[i][j].is_some() || cx.slots.contains(&next),
                        "case {n}: {next:?} is not the case's"
                    );
                }
            }
        }
        assert!(offered > 0, "the next-output option was never drawn");
        // The case's last slot's next output is the next case's first slot: never offered.
        let (d, cx) = base(5, 0, [0; 8]);
        let last = cx.slots[SLOTS - 1];
        assert!(!cx.slots.contains(&(last.0, last.1 + 1)));
        assert!(
            d.plan.txs.iter().all(|t| t.inputs.iter().all(|i| i.prevout != (last.0, last.1 + 1)))
        );
    }

    /// OD-L3-53: a draw the regime filter refuses is finalized and checked first. Its dropped
    /// second mutation, a re-root over its merkle value, or a finalization that fails is a
    /// harness error, never a `Tried` refusal or a redraw; done right, it is a typed refusal.
    #[test]
    fn a_refused_draw_is_finalized_and_checked_first() {
        let (d, cx, built) = wide_base();
        let origin = d.origin[1];
        let qc = Mutation {
            family: "tx.version",
            target: Target::Tx(origin),
            field: Field::TxVersion,
            new: Val::Bytes(tx_version(0, 6)),
        };
        let mut refused = d.clone();
        (refused.plan.txs[1].version, refused.plan.txs[1].tx_type) = (0, 6);
        let ok = crafted(&d, refused.clone(), vec![qc.clone()]);
        match pick_one(&d, &cx, &built, &ok) {
            Ok(Outcome::Filtered {
                tried,
            }) => {
                assert_eq!(tried.len(), DRAWS as usize);
                assert!(
                    tried
                        .iter()
                        .all(|t| t.reason.starts_with(&format!("filtered: {QC_TYPE_PRE_DIP3}")))
                );
            }
            other => panic!("{other:?}"),
        }
        // (a) A dropped second mutation: hdr.time recorded, the draft's time the base's.
        let mut dropped = ok.clone();
        dropped.draft.plan.time = d.plan.time;
        assert!(pick_one(&d, &cx, &built, &dropped).is_err(), "a dropped mutation was a refusal");
        // (a) A re-root over a recorded merkle value.
        let mut rerooted = ok.clone();
        rerooted.mutations.push(Mutation {
            family: "hdr.merkle",
            target: Target::Header,
            field: Field::Merkle,
            new: Val::Bytes(vec![0x5a; 32]),
        });
        assert!(pick_one(&d, &cx, &built, &rerooted).is_err(), "a re-root was a refusal");
        let mut fixed = rerooted.clone();
        fixed.draft.merkle = Merkle::Fixed([0x5a; 32]);
        assert!(matches!(pick_one(&d, &cx, &built, &fixed), Ok(Outcome::Filtered { .. })));
        // (b) A finalization that fails (a link cycle) on the refused draw.
        let mut cycle = ok;
        cycle.draft.links[1][0] = Some((origin, 0));
        let e = pick_one(&d, &cx, &built, &cycle).expect_err("a failed finalization was a refusal");
        assert!(e.contains("cycle"), "{e}");
    }

    /// `hdr.bits`: a value a grind meets is ground against itself; the others are left
    /// unground (nonce 0), and regtest's own limit is grindable.
    #[test]
    fn bits_are_ground_or_left_unground() {
        assert!(grindable(REGTEST_BITS) && grindable(REGTEST_BITS - 1) && grindable(0x207f_0000));
        // 2^248: a grind meets it in about 2^8 tries.
        assert!(grindable(0x2000_ffff));
        for bits in [0x2080_0000, 0x217f_ffff, 0x1d00_ffff, 0x1f00_ffff, 0, 0x2000_0000] {
            assert!(!grindable(bits), "{bits:08x}");
        }
        let (mut d, _) = base(5, 3, [3; 8]);
        d.plan.bits = REGTEST_BITS - 1;
        let (_, built) = d.finish().expect("ground");
        assert!(crate::chain::build::meets_bits(&built.hash, REGTEST_BITS - 1));
        d.plan.bits = 0x1d00_ffff;
        d.grind = false;
        let (_, built) = d.finish().expect("unground");
        assert_eq!(built.header[76..80], [0; 4], "nonce 0");
    }

    /// Spend chains follow their parent: a mutation of a parent re-links its child to the
    /// parent's final txid; a cut link (a `tx.prevout` mutation) stays where it was set.
    #[test]
    fn links_follow_their_parent_to_the_final_block() {
        let n = (0..200).find(|&n| {
            let (d, _) = base(5, n, [4; 8]);
            d.links.iter().flatten().any(Option::is_some)
        });
        let n = n.expect("a population with a chain");
        let (mut d, _) = base(5, n, [4; 8]);
        let (i, j, parent) = d
            .links
            .iter()
            .enumerate()
            .find_map(|(i, l)| {
                l.iter().position(Option::is_some).map(|j| (i, j, l[j].expect("a link").0))
            })
            .expect("a link");
        let p = d.index(parent).expect("the parent");
        d.plan.txs[p].lock_time = 5;
        let (plan, _) = d.finish().expect("final");
        assert_eq!(plan.txs[i].inputs[j].prevout.0, plan.txs[p].txid());
    }
}
